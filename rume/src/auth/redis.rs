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
}
