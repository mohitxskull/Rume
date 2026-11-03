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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    /// Unique session identifier (e.g. cookie value).
    pub id: String,
    /// Associated user or subject identifier.
    pub user_id: String,
    /// Roles associated with this session.
    pub roles: Vec<String>,
    /// Permissions associated with this session.
    pub permissions: Vec<String>,
    /// Epoch timestamp when this session expires.
    pub expires_at: u64,
    /// Arbitrary session payload data.
    pub data: BTreeMap<String, String>,
}

impl Session {
    /// Creates a new session with an expiration duration from now.
    pub fn new(id: impl Into<String>, user_id: impl Into<String>, ttl_secs: u64) -> Self {
        Self {
            id: id.into(),
            user_id: user_id.into(),
            roles: Vec::new(),
            permissions: Vec::new(),
            expires_at: current_timestamp() + ttl_secs,
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

    /// Rotates a session identifier to prevent session fixation, copying existing data to the new ID and deleting the old ID.
    fn regenerate_id<'a>(&'a self, old_id: &'a str) -> BoxFuture<'a, Result<Session, AuthError>> {
        Box::pin(async move {
            let existing = self.load(old_id).await?.ok_or_else(|| {
                AuthError::InvalidCredentials("cannot regenerate nonexistent session".into())
            })?;
            let new_session = Session {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: existing.user_id,
                roles: existing.roles,
                permissions: existing.permissions,
                data: existing.data,
                expires_at: existing.expires_at,
            };
            self.save(new_session.clone()).await?;
            if let Err(err) = self.destroy(old_id).await {
                tracing::warn!(error = %err, old_id = %old_id, "Failed to destroy old session during id regeneration");
            }
            Ok(new_session)
        })
    }
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
            let map = self.sessions.read().await;
            if let Some(session) = map.get(&id) {
                if session.is_expired() {
                    Ok(None)
                } else {
                    Ok(Some(session.clone()))
                }
            } else {
                Ok(None)
            }
        })
    }

    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>> {
        Box::pin(async move {
            let mut map = self.sessions.write().await;
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
}
