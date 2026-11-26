use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use crate::jobs::error::JobError;
use crate::jobs::job::BoxFuture;
use crate::jobs::reliability::{IdempotencyStatus, IdempotencyStore};
use rume_data::cache::redis::RedisCacheBackend;

const COMPLETE_WITH_TOKEN_SCRIPT: &str = r#"
local current = redis.call('GET', KEYS[1])
local matches = false
if current and ARGV[3] == '' and current == 'IN_PROGRESS' then
    matches = true
elseif current and ARGV[3] ~= '' and current == ('IN_PROGRESS:' .. ARGV[3]) then
    matches = true
end

if matches then
    redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
    return 1
else
    return 0
end
"#;

const RELEASE_WITH_TOKEN_SCRIPT: &str = r#"
local current = redis.call('GET', KEYS[1])
local can_delete = false
if current and ARGV[1] == '' and current == 'IN_PROGRESS' then
    can_delete = true
elseif current and ARGV[1] ~= '' and current == ('IN_PROGRESS:' .. ARGV[1]) then
    can_delete = true
elseif current and ARGV[1] == '' and string.sub(current, 1, 9) == 'COMPLETED' then
    can_delete = true
end

if can_delete then
    redis.call('DEL', KEYS[1])
    return 1
else
    return 0
end
"#;

const EXTEND_LOCK_WITH_TOKEN_SCRIPT: &str = r#"
local current = redis.call('GET', KEYS[1])
local matches = false
if current and ARGV[1] ~= '' and current == ('IN_PROGRESS:' .. ARGV[1]) then
    matches = true
elseif current and ARGV[1] == '' and current == 'IN_PROGRESS' then
    matches = true
end

if matches then
    redis.call('PEXPIRE', KEYS[1], ARGV[2])
    return 1
else
    return 0
end
"#;

/// Redis-backed implementation of [`IdempotencyStore`].
///
/// Guarantees cluster-wide idempotency locking and cached execution results across
/// multiple application instances.
#[derive(Clone)]
pub struct RedisIdempotencyStore {
    pool: Pool,
    prefix: String,
}

impl Debug for RedisIdempotencyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisIdempotencyStore")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisIdempotencyStore {
    /// Creates a new idempotency store from a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "rume:idempotency:".to_string(),
        }
    }

    /// Creates an idempotency store from a [`RedisCacheBackend`].
    pub fn from_backend(backend: &RedisCacheBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
            prefix: "rume:idempotency:".to_string(),
        }
    }

    /// Configures a custom key prefix for idempotency key isolation.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Formats an idempotency key with length-prefixed tenant isolation to prevent collisions.
    pub fn scoped_key(tenant_id: &str, key: &str) -> String {
        crate::jobs::reliability::scoped_idempotency_key(tenant_id, key)
    }

    fn key(&self, key: &str) -> String {
        format!("{}{{{}}}", self.prefix, key)
    }
}

impl IdempotencyStore for RedisIdempotencyStore {
    fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
        self.try_acquire_with_token(key, "", ttl)
    }

    fn try_acquire_with_token(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
        let redis_key = self.key(key);
        let token = token.to_string();
        let ttl_ms = ttl.as_millis().max(1) as u64;
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Lua script:
            // 1. If key doesn't exist, set 'IN_PROGRESS' (or 'IN_PROGRESS:<token>') with TTL, return {1, nil} (New)
            // 2. If key is 'IN_PROGRESS' or starts with 'IN_PROGRESS:', return {2, nil} (InProgress)
            // 3. If key starts with 'COMPLETED:', return {3, payload} (Completed)
            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if not current then
                    local val = 'IN_PROGRESS'
                    if ARGV[2] ~= '' then
                        val = 'IN_PROGRESS:' .. ARGV[2]
                    end
                    redis.call('SET', KEYS[1], val, 'PX', ARGV[1])
                    return { 1, false }
                else
                    if current == 'IN_PROGRESS' or string.sub(current, 1, 12) == 'IN_PROGRESS:' then
                        return { 2, false }
                    else
                        return { 3, current }
                    end
                end
                "#,
            );

            let res: (i64, redis::Value) = script
                .key(&redis_key)
                .arg(ttl_ms)
                .arg(&token)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            match res.0 {
                1 => Ok(IdempotencyStatus::New),
                2 => Ok(IdempotencyStatus::InProgress),
                3 => {
                    let raw_str = match res.1 {
                        redis::Value::BulkString(bytes) => {
                            String::from_utf8(bytes).unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    if let Some(hex_payload) = raw_str.strip_prefix("COMPLETED:") {
                        let bytes = if hex_payload.is_empty() {
                            None
                        } else {
                            hex_decode(hex_payload)
                        };
                        Ok(IdempotencyStatus::Completed(bytes))
                    } else {
                        Ok(IdempotencyStatus::Completed(None))
                    }
                }
                _ => Ok(IdempotencyStatus::InProgress),
            }
        })
    }

    fn complete(&self, key: &str, result: Option<Vec<u8>>) -> BoxFuture<'_, Result<(), JobError>> {
        self.complete_with_token(key, "", result)
    }

    fn complete_with_token(
        &self,
        key: &str,
        token: &str,
        result: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let redis_key = self.key(key);
        let key = key.to_string();
        let token = token.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let mut val = "COMPLETED:".to_string();
            if let Some(payload) = result {
                val.push_str(&hex_encode(&payload));
            }

            // Retain completed state for 24 hours (86,400,000 ms), but only transition if currently IN_PROGRESS.
            // Why: an empty token (`ARGV[3] == ''`) may ONLY match an unfenced
            // bare `'IN_PROGRESS'` lock, and a non-empty fencing token (`ARGV[3] ~= ''`)
            // may ONLY match `'IN_PROGRESS:' .. ARGV[3]`, never a bare `'IN_PROGRESS'` lock.
            let ttl_ms = 86_400_000_u64;
            let script = redis::Script::new(COMPLETE_WITH_TOKEN_SCRIPT);

            let res: i64 = script
                .key(&redis_key)
                .arg(&val)
                .arg(ttl_ms)
                .arg(&token)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res == 0 {
                return Err(JobError::IdempotencyLockLost(format!(
                    "failed to complete idempotency key '{key}': token mismatch or lease expired"
                )));
            }

            Ok(())
        })
    }

    fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>> {
        self.release_with_token(key, "")
    }

    fn release_with_token(&self, key: &str, token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let redis_key = self.key(key);
        let key = key.to_string();
        let token = token.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Evicts key if bare IN_PROGRESS (with empty token), matching IN_PROGRESS:<token>, or COMPLETED (with empty token).
            // Why: empty token (`ARGV[1] == ''`) must never break a fenced `IN_PROGRESS:<token>` lock,
            // and a non-empty fencing token (`ARGV[1] ~= ''`) must never release an unfenced bare `IN_PROGRESS` lock.
            let script = redis::Script::new(RELEASE_WITH_TOKEN_SCRIPT);

            let res: i64 = script
                .key(&redis_key)
                .arg(&token)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res == 0 {
                return Err(JobError::IdempotencyLockLost(format!(
                    "failed to release idempotency key '{key}': token mismatch or lease expired"
                )));
            }

            Ok(())
        })
    }

    fn extend_lock_with_token(
        &self,
        key: &str,
        token: &str,
        extension: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let redis_key = self.key(key);
        let key = key.to_string();
        let token = token.to_string();
        let extension_ms = extension.as_millis().max(1) as u64;
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Why: empty token (`ARGV[1] == ''`) may only extend an unfenced
            // bare `'IN_PROGRESS'` lock, never a fenced `'IN_PROGRESS:<token>'` lock.
            let script = redis::Script::new(EXTEND_LOCK_WITH_TOKEN_SCRIPT);

            let res: i64 = script
                .key(&redis_key)
                .arg(&token)
                .arg(extension_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res == 0 {
                return Err(JobError::IdempotencyLockLost(format!(
                    "failed to extend idempotency key '{key}': token mismatch or lease expired"
                )));
            }

            Ok(())
        })
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_scripts_fence_bare_in_progress_entries_with_empty_token_guard() {
        assert!(
            COMPLETE_WITH_TOKEN_SCRIPT.contains("ARGV[3] == '' and current == 'IN_PROGRESS'"),
            "complete_with_token must guard bare IN_PROGRESS with ARGV[3] == ''"
        );
        assert!(
            !COMPLETE_WITH_TOKEN_SCRIPT.contains("if current == 'IN_PROGRESS' then"),
            "complete_with_token must not allow a non-empty token to match bare IN_PROGRESS"
        );

        assert!(
            RELEASE_WITH_TOKEN_SCRIPT.contains("ARGV[1] == '' and current == 'IN_PROGRESS'"),
            "release_with_token must guard bare IN_PROGRESS with ARGV[1] == ''"
        );
        assert!(
            !RELEASE_WITH_TOKEN_SCRIPT.contains("if current == 'IN_PROGRESS' then"),
            "release_with_token must not allow a non-empty token to match bare IN_PROGRESS"
        );

        assert!(
            EXTEND_LOCK_WITH_TOKEN_SCRIPT.contains("ARGV[1] == '' and current == 'IN_PROGRESS'"),
            "extend_lock_with_token must guard bare IN_PROGRESS with ARGV[1] == ''"
        );
    }
}
