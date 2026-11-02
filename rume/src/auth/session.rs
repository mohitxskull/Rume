//! Secure session management and in-memory store.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
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

    /// Checks if the session has expired.
    pub fn is_expired(&self) -> bool {
        current_timestamp() >= self.expires_at
    }

    /// Adds a role to the session.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Adds a permission to the session.
    pub fn with_permission(mut self, perm: impl Into<String>) -> Self {
        self.permissions.push(perm.into());
        self
    }
}

/// Trait for storing, retrieving, and destroying authenticated sessions.
pub trait SessionStore: Send + Sync + 'static {
    /// Retrieves a session by its identifier.
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>>;

    /// Saves or updates a session.
    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>>;

    /// Invalidates and removes a session.
    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>>;
}

/// Thread-safe in-memory session store for development, testing, and single-node setups.
#[derive(Debug, Default, Clone)]
pub struct MemorySessionStore {
    sessions: Arc<RwLock<BTreeMap<String, Session>>>,
}

impl MemorySessionStore {
    /// Creates a new in-memory session store.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Returns the number of currently active non-expired sessions.
    pub fn active_count(&self) -> usize {
        if let Ok(sessions) = self.sessions.read() {
            sessions.values().filter(|s| !s.is_expired()).count()
        } else {
            0
        }
    }
}

impl SessionStore for MemorySessionStore {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>> {
        let sid = id.to_string();
        Box::pin(async move {
            let sessions = self
                .sessions
                .read()
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;

            if let Some(session) = sessions.get(&sid) {
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
            let mut sessions = self
                .sessions
                .write()
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            sessions.insert(session.id.clone(), session);
            Ok(())
        })
    }

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>> {
        let sid = id.to_string();
        Box::pin(async move {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|e| AuthError::ProviderError(e.to_string()))?;
            sessions.remove(&sid);
            Ok(())
        })
    }
}
