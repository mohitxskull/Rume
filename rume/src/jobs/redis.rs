//! Production Redis-backed worker heartbeats and idempotency storage.

use std::fmt::Debug;
use std::time::Duration;

use deadpool_redis::Pool;

use crate::cache::redis::RedisCacheBackend;
use crate::jobs::error::JobError;
use crate::jobs::job::BoxFuture;
use crate::jobs::reliability::{HeartbeatRegistry, IdempotencyStatus, IdempotencyStore};

fn current_epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Redis-backed implementation of [`HeartbeatRegistry`].
///
/// Uses Redis Sorted Sets (`ZADD`, `ZSCORE`, `ZRANGEBYSCORE`) to track active worker nodes
/// across a cluster and reliably detect/reap deceased workers.
#[derive(Clone)]
pub struct RedisHeartbeatRegistry {
    pool: Pool,
    set_key: String,
}

impl Debug for RedisHeartbeatRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisHeartbeatRegistry")
            .field("set_key", &self.set_key)
            .finish()
    }
}

impl RedisHeartbeatRegistry {
    /// Creates a new heartbeat registry from a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            set_key: "rume:jobs:heartbeats".to_string(),
        }
    }

    /// Creates a heartbeat registry from a [`RedisCacheBackend`].
    pub fn from_backend(backend: &RedisCacheBackend) -> Self {
        Self {
            pool: backend.pool().clone(),
            set_key: "rume:jobs:heartbeats".to_string(),
        }
    }

    /// Configures a custom key for heartbeat tracking.
    pub fn with_set_key(mut self, key: impl Into<String>) -> Self {
        self.set_key = key.into();
        self
    }
}

impl HeartbeatRegistry for RedisHeartbeatRegistry {
    fn record_heartbeat(
        &self,
        worker_id: &str,
        ttl: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();
            let expires_at = now.saturating_add(ttl.as_millis() as i64);

            let _: () = redis::cmd("ZADD")
                .arg(&self.set_key)
                .arg(expires_at)
                .arg(&wid)
                .query_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn is_alive(&self, worker_id: &str) -> BoxFuture<'_, Result<bool, JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();
            let score: Option<i64> = redis::cmd("ZSCORE")
                .arg(&self.set_key)
                .arg(&wid)
                .query_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(exp) = score {
                Ok(exp > now)
            } else {
                Ok(false)
            }
        })
    }

    fn reap_dead_workers(&self) -> BoxFuture<'_, Result<Vec<String>, JobError>> {
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();

            // Lua script: query dead worker IDs with score <= now, remove them atomically, and return list
            let script = redis::Script::new(
                r#"
                local dead = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                if #dead > 0 then
                    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                end
                return dead
                "#,
            );

            let dead_workers: Vec<String> = script
                .key(&self.set_key)
                .arg(now)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(dead_workers)
        })
    }
}

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
        let redis_key = self.key(key);
        let ttl_ms = ttl.as_millis().max(1) as u64;
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Lua script:
            // 1. If key doesn't exist, set 'IN_PROGRESS' with TTL, return {1, nil} (New)
            // 2. If key is 'IN_PROGRESS', return {2, nil} (InProgress)
            // 3. If key starts with 'COMPLETED:', return {3, payload} (Completed)
            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if not current then
                    redis.call('SET', KEYS[1], 'IN_PROGRESS', 'PX', ARGV[1])
                    return { 1, false }
                else
                    if current == 'IN_PROGRESS' then
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
        let redis_key = self.key(key);
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

            // Retain completed state for 24 hours (86,400,000 ms), but only transition if currently IN_PROGRESS
            let ttl_ms = 86_400_000_u64;
            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current == 'IN_PROGRESS' then
                    redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
                    return 1
                else
                    return 0
                end
                "#,
            );

            let _: i64 = script
                .key(&redis_key)
                .arg(&val)
                .arg(ttl_ms)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn release(&self, key: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let redis_key = self.key(key);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            // Only delete the lock if it is still IN_PROGRESS, preventing deletion of completed jobs or peer claims
            let script = redis::Script::new(
                r#"
                local current = redis.call('GET', KEYS[1])
                if current == 'IN_PROGRESS' then
                    redis.call('DEL', KEYS[1])
                    return 1
                else
                    return 0
                end
                "#,
            );

            let _: i64 = script
                .key(&redis_key)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

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
