//! SQL-backed [`SessionStore`] implementation.

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use rume_data::persistence::DatabaseBackend;

use crate::auth::error::AuthError;
use crate::auth::session::{BoxFuture, Session, SessionStore};

/// Persistent SQL [`SessionStore`] across SQLite, PostgreSQL, and MySQL.
#[derive(Debug, Clone)]
pub struct SqlSessionStore<B: DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: DatabaseBackend> SqlSessionStore<B> {
    /// Creates a new [`SqlSessionStore`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Deletes all expired sessions as of `now_epoch_sec`, returning the number removed.
    pub async fn prune_expired(&self, now_epoch_sec: u64) -> Result<u64, AuthError> {
        todo!("implemented in Task 04")
    }
}

impl<B: DatabaseBackend> SessionStore for SqlSessionStore<B> {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<Session>, AuthError>> {
        todo!("implemented in Task 04")
    }

    fn save(&self, session: Session) -> BoxFuture<'_, Result<(), AuthError>> {
        todo!("implemented in Task 04")
    }

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<(), AuthError>> {
        todo!("implemented in Task 04")
    }

    fn regenerate_id(&self, old_id: &str) -> BoxFuture<'_, Result<Session, AuthError>> {
        todo!("implemented in Task 04")
    }
}
