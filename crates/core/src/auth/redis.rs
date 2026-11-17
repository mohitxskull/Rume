//! Production Redis-backed session storage.

use std::fmt::Debug;

use deadpool_redis::Pool;

use crate::auth::error::AuthError;
use crate::auth::session::{BoxFuture, Session, SessionStore};

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Escape a string for safe use as a Lua *pattern* (not literal) in
/// `string.match`/`string.gsub`. Lua pattern magic characters
/// (`^ $ ( ) % . [ ] * + - ?`) are prefixed with `%` so they match
/// literally. This prevents hyphenated UUIDs (e.g. session IDs) from
/// being misinterpreted as pattern operators.
fn escape_lua_pattern(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '^' | '$' | '(' | ')' | '%' | '.' | '[' | ']' | '*' | '+' | '-' | '?' => {
                result.push('%');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}

/// Escape `%` in a string used as a Lua *replacement* (in `string.gsub`),
/// where `%` introduces capture references (`%0`..`%9`, `%1`..`%9`).
fn escape_lua_replacement(s: &str) -> String {
    s.replace('%', "%%")
}

/// Redis-backed implementation of [`SessionStore`].
///
/// Stores JSON-serialized sessions with Redis TTL auto-expiration matching [`Session::expires_at`].
#[derive(Clone)]
pub struct RedisSessionStore {
    pool: Pool,
    prefix: String,
}

impl Debug for RedisSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSessionStore")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisSessionStore {
    /// Creates a new session store using a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "rume:session:".to_string(),
        }
    }

    /// Configures a custom key prefix for session isolation.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    fn key(&self, id: &str) -> String {
        format!("{}{}", self.prefix, id)
    }
}

impl SessionStore for RedisSessionStore {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>> {
        let redis_key = self.key(id);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let data: Option<Vec<u8>> = redis::cmd("GET")
                .arg(&redis_key)
                .query_async(&mut *conn)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            if let Some(bytes) = data {
                let session: Session = serde_json::from_slice(&bytes)
                    .map_err(|e| AuthError::ProviderError(format!("malformed session: {e}")))?;
                if session.is_expired() {
                    Ok(None)
                } else {
                    Ok(Some(session))
                }
            } else {
                Ok(None)
            }
        })
    }

    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>> {
        let redis_key = self.key(&session.id);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let now = current_timestamp();
            let ttl_secs = session.expires_at.saturating_sub(now).max(1);

            let bytes = serde_json::to_vec(&session).map_err(|e| {
                AuthError::ProviderError(format!("failed to serialize session: {e}"))
            })?;

            let _: () = redis::cmd("SET")
                .arg(&redis_key)
                .arg(&bytes)
                .arg("EX")
                .arg(ttl_secs)
                .query_async(&mut *conn)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            Ok(())
        })
    }

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>> {
        let redis_key = self.key(id);
        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let _: () = redis::cmd("DEL")
                .arg(&redis_key)
                .query_async(&mut *conn)
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            Ok(())
        })
    }

    fn regenerate_id<'a>(&'a self, old_id: &'a str) -> BoxFuture<'a, Result<Session, AuthError>> {
        let old_key = self.key(old_id);
        let new_id = uuid::Uuid::new_v4().to_string();
        let new_key = self.key(&new_id);
        // Escape old_id for use as a Lua pattern (UUID hyphens are pattern
        // operators, not literals). Escape new_id for use as a replacement
        // string (% introduces capture references).
        let old_id_pat = escape_lua_pattern(old_id);
        let new_id_rep = escape_lua_replacement(&new_id);

        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let now = current_timestamp();

            let script = format!(
                r#"
                local old_key = KEYS[1]
                local new_key = KEYS[2]
                local old_id_pat = ARGV[1]
                local new_id_rep = ARGV[2]
                local now = tonumber(ARGV[3])

                local data = redis.call('GET', old_key)
                if not data then
                    return nil
                end

                local exp_str = string.match(data, '"expires_at":%s*(%d+)')
                if exp_str and tonumber(exp_str) <= now then
                    redis.call('DEL', old_key)
                    return redis.error_reply("EXPIRED")
                end

                -- Replace "id":"<old_id>" with "id":"<new_id>" using plain
                -- string search (not pattern matching) to avoid Lua pattern
                -- magic characters in UUIDs (e.g. hyphens).
                local start, stop = string.find(data, '"id":"' .. old_id_pat .. '"', 1, true)
                local updated
                if start then
                    updated = string.sub(data, 1, start - 1)
                        .. '"id":"' .. new_id_rep .. '"'
                        .. string.sub(data, stop + 1)
                else
                    -- Fallback: try with optional whitespace after colon
                    local target = '"id":%s*"' .. old_id_pat .. '"'
                    local replacement = '"id":"' .. new_id_rep .. '"'
                    updated, _ = string.gsub(data, target, replacement, 1)
                end

                local ttl = redis.call('PTTL', old_key)
                if ttl > 0 then
                    redis.call('SET', new_key, updated, 'PX', ttl)
                else
                    redis.call('SET', new_key, updated)
                end
                redis.call('DEL', old_key)
                return updated
                "#,
            );

            let res: Result<Option<Vec<u8>>, redis::RedisError> = redis::Script::new(&script)
                .key(&old_key)
                .key(&new_key)
                .arg(&old_id_pat)
                .arg(&new_id_rep)
                .arg(now)
                .invoke_async(&mut *conn)
                .await;

            let data = match res {
                Ok(opt) => opt,
                Err(err) if err.to_string().contains("EXPIRED") => {
                    return Err(AuthError::InvalidCredentials("session expired".into()));
                }
                Err(err) => return Err(AuthError::ProviderError(err.to_string())),
            };

            let bytes = data.ok_or_else(|| {
                AuthError::InvalidCredentials("cannot regenerate nonexistent session".into())
            })?;

            let session: Session = serde_json::from_slice(&bytes)
                .map_err(|e| AuthError::ProviderError(format!("malformed session: {e}")))?;

            Ok(session)
        })
    }
}
