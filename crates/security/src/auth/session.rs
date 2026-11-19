//! Secure session management and storage contract.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::auth::error::AuthError;

/// Pinned boxed future type alias for async session store operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// An active authenticated user session.
///
/// Why: `tenant_id`, `roles`, `permissions`, and `data` carry `#[serde(default)]` because Redis
/// Lua's `cjson` library cannot distinguish empty JSON arrays `[]` from empty objects `{}` and
/// encodes empty Lua tables as `{}`; omitting empty optional collections in Lua during session
/// rotation while keeping `id`, `user_id`, and `expires_at` mandatory preserves structural JSON
/// round-tripping without string-hacking.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    /// Unique session identifier (e.g. cookie value).
    pub id: String,
    /// Associated user or subject identifier.
    pub user_id: String,
    /// Tenant this session is bound to (None for non-tenant sessions).
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Roles associated with this session.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Permissions associated with this session.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Epoch timestamp when this session expires.
    pub expires_at: u64,
    /// Arbitrary session payload data.
    #[serde(default)]
    pub data: BTreeMap<String, String>,
}

impl Session {
    /// Creates a new session with an expiration duration from now.
    pub fn new(id: impl Into<String>, user_id: impl Into<String>, ttl_secs: u64) -> Self {
        Self {
            id: id.into(),
            user_id: user_id.into(),
            tenant_id: None,
            roles: Vec::new(),
            permissions: Vec::new(),
            expires_at: current_timestamp().saturating_add(ttl_secs),
            data: BTreeMap::new(),
        }
    }

    /// Checks if this session has passed its expiration timestamp.
    pub fn is_expired(&self) -> bool {
        current_timestamp() >= self.expires_at
    }

    /// Appends a role to this session.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Appends a permission to this session.
    pub fn with_permission(mut self, permission: impl Into<String>) -> Self {
        self.permissions.push(permission.into());
        self
    }

    /// Binds this session to a tenant.
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Inserts a key-value pair into arbitrary session data.
    pub fn with_data(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.data.insert(key.into(), value.into());
        self
    }
}

/// Interface for session persistence drivers.
pub trait SessionStore: Send + Sync + 'static {
    /// Loads a session by identifier. Returns `None` if absent or expired.
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>>;

    /// Saves or updates a session.
    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>>;

    /// Invalidates and removes a session.
    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>>;

    /// Atomically rotates a session identifier to prevent session fixation, copying existing data
    /// to the new ID and deleting the old ID.
    ///
    /// Why: required trait method (no default `load -> save -> destroy` body) because a
    /// non-atomic default permits logout resurrection (TOCTOU) and leaves pre-rotation IDs valid
    /// if `destroy` fails.
    fn regenerate_id<'a>(&'a self, old_id: &'a str) -> BoxFuture<'a, Result<Session, AuthError>>;
}

/// Thread-safe in-memory session store suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemorySessionStore {
    sessions: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, Session>>>,
}

impl InMemorySessionStore {
    /// Creates a new in-memory session store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for InMemorySessionStore {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>> {
        let id = id.to_string();
        Box::pin(async move {
            let expired = {
                let map = self.sessions.read().await;
                if let Some(session) = map.get(&id) {
                    if session.is_expired() {
                        true
                    } else {
                        return Ok(Some(session.clone()));
                    }
                } else {
                    return Ok(None);
                }
            };
            if expired {
                let mut map = self.sessions.write().await;
                if map.get(&id).is_some_and(Session::is_expired) {
                    map.remove(&id);
                }
            }
            Ok(None)
        })
    }

    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>> {
        Box::pin(async move {
            let mut map = self.sessions.write().await;
            map.retain(|_, s| !s.is_expired());
            map.insert(session.id.clone(), session);
            Ok(())
        })
    }

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>> {
        let id = id.to_string();
        Box::pin(async move {
            let mut map = self.sessions.write().await;
            map.remove(&id);
            Ok(())
        })
    }

    /// Rotates a session identifier atomically: load → save → destroy all
    /// under a single write lock, so no interleaved destroy can resurrect
    /// the old id (session-fixation race).
    fn regenerate_id<'a>(&'a self, old_id: &'a str) -> BoxFuture<'a, Result<Session, AuthError>> {
        let old_id = old_id.to_string();
        Box::pin(async move {
            let mut map = self.sessions.write().await;
            let Some(existing) = map.get(&old_id).cloned() else {
                return Err(AuthError::InvalidCredentials(
                    "cannot regenerate nonexistent session".into(),
                ));
            };

            if existing.is_expired() {
                map.remove(&old_id);
                return Err(AuthError::InvalidCredentials(
                    "cannot regenerate expired session".into(),
                ));
            }

            map.retain(|_, s| !s.is_expired());

            let new_session = Session {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: existing.user_id,
                tenant_id: existing.tenant_id.clone(),
                roles: existing.roles,
                permissions: existing.permissions,
                data: existing.data,
                expires_at: existing.expires_at,
            };
            map.insert(new_session.id.clone(), new_session.clone());
            map.remove(&old_id);
            Ok(new_session)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_session_store_evicts_expired_sessions() {
        let store = InMemorySessionStore::new();
        let mut expired = Session::new("s-old", "u1", 0);
        expired.expires_at = 0;
        store.save(expired).await.unwrap();
        assert!(store.load("s-old").await.unwrap().is_none());
        assert_eq!(store.sessions.read().await.len(), 0);
    }

    #[test]
    fn session_new_saturates_expiry_on_overflow() {
        let s = Session::new("s1", "u1", u64::MAX);
        assert_eq!(s.expires_at, u64::MAX);
    }
}
