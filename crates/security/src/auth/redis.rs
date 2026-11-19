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
        return redis.error_reply("EXPIRED")
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
        redis.call('SET', new_key, updated)
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
            "Lua script must validate expires_at before comparing and return MALFORMED"
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
}
