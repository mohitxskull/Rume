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

// Why: mutate the session JSON structurally via `cjson` instead of pattern-matching raw JSON
// strings, failing closed with `MALFORMED` on corrupt/non-object payloads or invalid `expires_at`.
// Empty `roles`/`permissions`/`data` tables are cleared to `nil` (paired with `#[serde(default)]`
// on `Session`) because Redis Lua's `cjson` encodes empty tables as `{}` rather than `[]`.
const REGENERATE_ID_SCRIPT: &str = r#"
    local old_key = KEYS[1]
    local new_key = KEYS[2]
    local new_id = ARGV[1]
    local now = tonumber(ARGV[2])

    local data = redis.call('GET', old_key)
    if not data then
        return nil
    end

    local ok, decoded = pcall(cjson.decode, data)
    if not ok
        or type(decoded) ~= 'table'
        or type(decoded.id) ~= 'string'
        or type(decoded.user_id) ~= 'string' then
        return redis.error_reply("MALFORMED")
    end

    local exp = tonumber(decoded.expires_at)
    if type(decoded.expires_at) ~= 'number' or not exp or not now then
        return redis.error_reply("MALFORMED")
    end
    if exp <= now then
        redis.call('DEL', old_key)
        -- Why: Redis 7.2/8.0 prepends `ERR ` to single-word `redis.error_reply("EXPIRED")`,
        -- which makes `redis-rs` report `err.code() == Some("ERR")` instead of `Some("EXPIRED")`.
        -- Including a message preserves the `EXPIRED` error code prefix.
        return redis.error_reply("EXPIRED session expired")
    end

    decoded.id = new_id
    if type(decoded.roles) == 'table' and next(decoded.roles) == nil then
        decoded.roles = nil
    end
    if type(decoded.permissions) == 'table' and next(decoded.permissions) == nil then
        decoded.permissions = nil
    end
    if type(decoded.data) == 'table' and next(decoded.data) == nil then
        decoded.data = nil
    end

    local updated = cjson.encode(decoded)
    local ttl = redis.call('PTTL', old_key)
    if ttl > 0 then
        redis.call('SET', new_key, updated, 'PX', ttl)
    else
        -- Why: if `old_key` was PERSISTed (`PTTL == -1`), writing `SET new_key updated`
        -- without a TTL would create an immortal session key. Fall back to the session's
        -- own remaining lifetime (min 1000ms).
        local fallback_ttl = math.max((exp - now) * 1000, 1000)
        redis.call('SET', new_key, updated, 'PX', fallback_ttl)
    end
    redis.call('DEL', old_key)
    return updated
"#;

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

/// Ensures a Redis key prefix carries a non-empty `{...}` cluster hash tag so multi-key
/// Lua scripts (`REGENERATE_ID_SCRIPT`) always map `KEYS[1]` and `KEYS[2]` to the same
/// Redis Cluster hash slot and never fail with `CROSSSLOT` (`RC-04`).
fn ensure_cluster_hash_tag(prefix: &str) -> String {
    if let Some(open) = prefix.find('{')
        && let Some(close_rel) = prefix[open + 1..].find('}')
        && close_rel > 0
    {
        return prefix.to_string();
    }
    let trimmed = prefix.trim_end_matches(':');
    if trimmed.is_empty() {
        "{rume:session}:".to_string()
    } else if prefix.ends_with(':') {
        format!("{{{trimmed}}}:")
    } else {
        format!("{{{trimmed}}}")
    }
}

impl RedisSessionStore {
    /// Creates a new session store using a `deadpool_redis::Pool`.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            prefix: "{rume:session}:".to_string(),
        }
    }

    /// Configures a custom key prefix for session isolation.
    ///
    /// Automatically wraps untagged prefixes in a Redis Cluster `{...}` hash tag so
    /// [`SessionStore::regenerate_id`] never triggers `CROSSSLOT`.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = ensure_cluster_hash_tag(&prefix.into());
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

        Box::pin(async move {
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            let now = current_timestamp();

            let res: Result<Option<Vec<u8>>, redis::RedisError> =
                redis::Script::new(REGENERATE_ID_SCRIPT)
                    .key(&old_key)
                    .key(&new_key)
                    .arg(&new_id)
                    .arg(now)
                    .invoke_async(&mut *conn)
                    .await;

            let data = match res {
                Ok(opt) => opt,
                Err(err) if err.code() == Some("EXPIRED") => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regenerate_id_script_uses_guarded_cjson_without_lua_pattern_helpers() {
        assert!(
            REGENERATE_ID_SCRIPT.contains("pcall(cjson.decode, data)"),
            "Lua script must decode JSON structurally with pcall(cjson.decode, data)"
        );
        assert!(
            REGENERATE_ID_SCRIPT.contains("tonumber(decoded.expires_at)")
                && REGENERATE_ID_SCRIPT.contains("MALFORMED"),
            "Lua script must validate expires_at numerically and reject malformed payloads"
        );
        assert!(
            REGENERATE_ID_SCRIPT.contains(r#"redis.error_reply("EXPIRED session expired")"#),
            "Lua script must emit a two-token EXPIRED error reply so Redis 7.2/8.0 preserves the EXPIRED code"
        );
        assert!(
            !REGENERATE_ID_SCRIPT.contains("string.gsub")
                && !REGENERATE_ID_SCRIPT.contains("string.find")
                && !REGENERATE_ID_SCRIPT.contains("string.match"),
            "Lua script must not rely on string.gsub/string.find/string.match"
        );

        let omitted_collections = r#"{"id":"s1","user_id":"u1","expires_at":9999999999}"#;
        let parsed: Session = serde_json::from_str(omitted_collections)
            .expect("Session must deserialize with omitted optional collections");
        assert_eq!(parsed.id, "s1");
        assert_eq!(parsed.user_id, "u1");
        assert_eq!(parsed.tenant_id, None);
        assert!(parsed.roles.is_empty());
        assert!(parsed.permissions.is_empty());
        assert!(parsed.data.is_empty());

        for required_missing in [
            r#"{"user_id":"u1","expires_at":9999999999}"#,
            r#"{"id":"s1","expires_at":9999999999}"#,
            r#"{"id":"s1","user_id":"u1"}"#,
        ] {
            assert!(
                serde_json::from_str::<Session>(required_missing).is_err(),
                "missing core field in {required_missing} must fail deserialization"
            );
        }
    }

    #[test]
    fn session_keys_share_identical_redis_cluster_hash_tag() {
        fn extract_hash_tag(key: &str) -> Option<&str> {
            let start = key.find('{')? + 1;
            let end = start + key[start..].find('}')?;
            (end > start).then_some(&key[start..end])
        }

        let pool = deadpool_redis::Config::from_url("redis://127.0.0.1:6379")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("pool");

        for store in [
            RedisSessionStore::new(pool.clone()),
            RedisSessionStore::new(pool.clone()).with_prefix("myapp:sess:"),
            RedisSessionStore::new(pool).with_prefix("ns:{custom-slot}:sess:"),
        ] {
            let k1 = store.key("old-session-id-1");
            let k2 = store.key("new-session-id-2");
            let t1 = extract_hash_tag(&k1).expect("old key must carry a non-empty hash tag");
            let t2 = extract_hash_tag(&k2).expect("new key must carry a non-empty hash tag");
            assert_eq!(t1, t2, "regenerate_id keys must hash to the same slot");
        }
    }
}
