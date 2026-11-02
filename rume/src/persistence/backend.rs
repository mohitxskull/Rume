//! Database backend abstraction and in-memory test implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use crate::outbox::{
    OutboxError, OutboxMessage, OutboxRepository, OutboxStatus, TransactionalOutbox,
};
use crate::persistence::error::PersistenceError;

/// Pinned boxed future type alias for async database traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Generic interface implemented by database drivers and storage backends.
pub trait DatabaseBackend: Debug + Send + Sync + 'static {
    /// Type of transaction produced by this backend.
    type Transaction: DatabaseTransaction + Clone + 'static;

    /// Verifies connectivity to the database.
    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Executes a SQL or DDL statement directly, returning the number of affected rows.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Executes a scalar query returning an optional string value.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Begins a new transaction.
    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>>;
}

/// Active transaction handle providing isolated mutation and explicit commit/rollback.
pub trait DatabaseTransaction: Debug + Send + Sync {
    /// Executes a statement within the transaction boundary.
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>>;

    /// Queries a scalar value within the transaction boundary.
    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>>;

    /// Atomically commits all changes made within this transaction.
    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;

    /// Explicitly discards and rolls back all changes made within this transaction.
    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>>;
}

#[derive(Debug, Default)]
struct MemoryDbState {
    kv_store: BTreeMap<String, String>,
    executed_statements: Vec<String>,
    applied_migrations: BTreeSet<String>,
    outbox: BTreeMap<String, OutboxMessage>,
    alive: bool,
}

/// An in-memory key-value test double and statement spy for unit tests.
///
/// > [!NOTE]
/// > `MemoryDatabase` is an in-memory test double designed for fast unit testing and interface
/// > validation. It does not parse full SQL or enforce relational constraints. For production
/// > workloads and genuine ANSI SQL testing, use `PostgresBackend` (with testcontainers) or
/// > `SqliteBackend`.
#[derive(Debug, Clone)]
pub struct MemoryDatabase {
    state: Arc<RwLock<MemoryDbState>>,
}

impl Default for MemoryDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryDatabase {
    /// Creates a new in-memory database instance.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(MemoryDbState {
                kv_store: BTreeMap::new(),
                executed_statements: Vec::new(),
                applied_migrations: BTreeSet::new(),
                outbox: BTreeMap::new(),
                alive: true,
            })),
        }
    }

    /// Sets a key-value pair directly in the in-memory test double.
    pub fn insert(&self, key: impl Into<String>, value: impl Into<String>) {
        if let Ok(mut state) = self.state.write() {
            state.kv_store.insert(key.into(), value.into());
        }
    }

    /// Gets a value by key directly from the in-memory test double.
    pub fn get(&self, key: &str) -> Option<String> {
        self.state
            .read()
            .ok()
            .and_then(|s| s.kv_store.get(key).cloned())
    }

    /// Removes a key from the in-memory test double.
    pub fn remove(&self, key: &str) -> Option<String> {
        self.state
            .write()
            .ok()
            .and_then(|mut s| s.kv_store.remove(key))
    }

    /// Returns a copy of all statements executed against this test double.
    pub fn executed_statements(&self) -> Vec<String> {
        self.state
            .read()
            .map(|s| s.executed_statements.clone())
            .unwrap_or_default()
    }

    /// Sets the alive flag, enabling simulation of database connection failures in health tests.
    pub fn set_alive(&self, alive: bool) {
        if let Ok(mut state) = self.state.write() {
            state.alive = alive;
        }
    }

    /// Returns whether the database is currently alive.
    pub fn is_alive(&self) -> bool {
        self.state.read().map(|s| s.alive).unwrap_or(false)
    }

    /// Internal helper to check if a migration has been applied.
    pub fn has_migration(&self, id: &str) -> bool {
        self.state
            .read()
            .map(|s| s.applied_migrations.contains(id))
            .unwrap_or(false)
    }

    /// Internal helper to mark a migration as applied.
    pub fn mark_migration(&self, id: &str) {
        if let Ok(mut state) = self.state.write() {
            state.applied_migrations.insert(id.to_string());
        }
    }

    /// Internal helper to remove a migration from applied set.
    pub fn unmark_migration(&self, id: &str) {
        if let Ok(mut state) = self.state.write() {
            state.applied_migrations.remove(id);
        }
    }
}

impl DatabaseBackend for MemoryDatabase {
    type Transaction = MemoryTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            if state.alive {
                Ok(())
            } else {
                Err(PersistenceError::ConnectionFailed(
                    "database offline".into(),
                ))
            }
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            if !state.alive {
                return Err(PersistenceError::Closed);
            }
            state.executed_statements.push(stmt);
            Ok(1)
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.trim().to_string();
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            if !state.alive {
                return Err(PersistenceError::Closed);
            }
            Ok(state.kv_store.get(&q).cloned())
        })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            if !state.alive {
                return Err(PersistenceError::Closed);
            }

            Ok(MemoryTransaction {
                state: Arc::clone(&self.state),
                staged: Arc::new(RwLock::new(MemoryStagedState::default())),
                status: Arc::new(AtomicU8::new(0)), // 0 = Active
            })
        })
    }
}

#[derive(Debug, Default)]
struct MemoryStagedState {
    kv: BTreeMap<String, Option<String>>,
    executed_statements: Vec<String>,
    outbox: BTreeMap<String, Option<OutboxMessage>>,
}

/// Active transaction for the in-memory database test double.
#[derive(Debug, Clone)]
pub struct MemoryTransaction {
    state: Arc<RwLock<MemoryDbState>>,
    staged: Arc<RwLock<MemoryStagedState>>,
    status: Arc<AtomicU8>, // 0: Active, 1: Committed, 2: RolledBack
}

impl MemoryTransaction {
    /// Enlists an outbox message into this in-memory transaction.
    ///
    /// The message will only be committed to the database if `self.commit()` succeeds.
    pub fn insert_outbox(&self, message: OutboxMessage) -> Result<(), PersistenceError> {
        if self.status.load(Ordering::SeqCst) != 0 {
            return Err(PersistenceError::TransactionError(
                "transaction already resolved".into(),
            ));
        }

        let mut staged = self
            .staged
            .write()
            .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
        staged.outbox.insert(message.id.clone(), Some(message));
        Ok(())
    }
}

impl DatabaseTransaction for MemoryTransaction {
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        Box::pin(async move {
            if self.status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }

            let mut staged = self
                .staged
                .write()
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;

            staged.executed_statements.push(stmt);
            Ok(1)
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.trim().to_string();
        Box::pin(async move {
            if self.status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }

            let staged = self
                .staged
                .read()
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            if let Some(val_opt) = staged.kv.get(&q) {
                return Ok(val_opt.clone());
            }
            let state = self
                .state
                .read()
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(state.kv_store.get(&q).cloned())
        })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            if self
                .status
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }

            let mut staged = self
                .staged
                .write()
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            let mut state = self
                .state
                .write()
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;

            for (k, v_opt) in std::mem::take(&mut staged.kv) {
                match v_opt {
                    Some(v) => {
                        state.kv_store.insert(k, v);
                    }
                    None => {
                        state.kv_store.remove(&k);
                    }
                }
            }
            state
                .executed_statements
                .extend(std::mem::take(&mut staged.executed_statements));
            for (id, opt_msg) in std::mem::take(&mut staged.outbox) {
                match opt_msg {
                    Some(msg) => {
                        state.outbox.insert(id, msg);
                    }
                    None => {
                        state.outbox.remove(&id);
                    }
                }
            }
            Ok(())
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            if let Ok(mut staged) = self.staged.write() {
                staged.kv.clear();
                staged.executed_statements.clear();
                staged.outbox.clear();
            }
            let _ = self
                .status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst);
            Ok(())
        })
    }
}

impl Drop for MemoryTransaction {
    fn drop(&mut self) {
        if Arc::strong_count(&self.status) <= 1 && self.status.load(Ordering::SeqCst) == 0 {
            if let Ok(mut staged) = self.staged.write() {
                staged.kv.clear();
                staged.executed_statements.clear();
                staged.outbox.clear();
            }
            let _ = self
                .status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst);
        }
    }
}

impl OutboxRepository for MemoryDatabase {
    fn save(&self, msg: OutboxMessage) -> BoxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            state.outbox.insert(msg.id.clone(), msg);
            Ok(())
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = crate::outbox::message::current_timestamp();
            let mut pending: Vec<OutboxMessage> = state
                .outbox
                .values()
                .filter(|m| {
                    m.status == OutboxStatus::Pending && m.next_retry_at.is_none_or(|t| now >= t)
                })
                .cloned()
                .collect();
            pending.sort_by_key(|m| m.created_at);
            pending.truncate(limit);
            Ok(pending)
        })
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = crate::outbox::message::current_timestamp();
            let mut pending_keys: Vec<(u64, String)> = state
                .outbox
                .values()
                .filter(|m| {
                    m.status == OutboxStatus::Pending && m.next_retry_at.is_none_or(|t| now >= t)
                })
                .map(|m| (m.created_at, m.id.clone()))
                .collect();
            pending_keys.sort_by_key(|(ts, _)| *ts);

            let mut claimed = Vec::new();
            for (_, id) in pending_keys.into_iter().take(limit) {
                if let Some(msg) = state.outbox.get_mut(&id) {
                    msg.status = OutboxStatus::InFlight;
                    msg.lease_token = Some(uuid::Uuid::new_v4().to_string());
                    msg.lease_expires_at = Some(now + lease_duration_secs);
                    claimed.push(msg.clone());
                }
            }
            Ok(claimed)
        })
    }

    fn mark_published(&self, id: &str) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if let Some(msg) = state.outbox.get_mut(&id_str) {
                msg.status = OutboxStatus::Published;
                msg.last_error = None;
                msg.next_retry_at = None;
                msg.lease_token = None;
                msg.lease_expires_at = None;
            }
            Ok(())
        })
    }

    fn mark_failed(&self, id: &str, error: String) -> BoxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if let Some(msg) = state.outbox.get_mut(&id_str) {
                msg.retry_count += 1;
                msg.last_error = Some(error);
                msg.lease_token = None;
                msg.lease_expires_at = None;
                let now = crate::outbox::message::current_timestamp();
                let backoff_secs = 2u64.pow(msg.retry_count.saturating_sub(1));
                msg.next_retry_at = Some(now + backoff_secs);

                if msg.retry_count >= 5 {
                    msg.status = OutboxStatus::Failed;
                } else {
                    msg.status = OutboxStatus::Pending;
                }
            }
            Ok(())
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = crate::outbox::message::current_timestamp();
            let mut recovered = 0;
            for msg in state.outbox.values_mut() {
                if msg.status == OutboxStatus::InFlight {
                    let expired = match msg.lease_expires_at {
                        Some(exp) => now >= exp,
                        None => now >= msg.created_at + max_lease_age_secs,
                    };
                    if expired {
                        msg.status = OutboxStatus::Pending;
                        msg.lease_token = None;
                        msg.lease_expires_at = None;
                        recovered += 1;
                    }
                }
            }
            Ok(recovered)
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let now = crate::outbox::message::current_timestamp();
            Ok(state
                .outbox
                .values()
                .filter(|m| {
                    (m.status == OutboxStatus::Pending && m.next_retry_at.is_none_or(|t| now >= t))
                        || m.status == OutboxStatus::InFlight
                })
                .count())
        })
    }

    fn published_count(&self) -> BoxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(state
                .outbox
                .values()
                .filter(|m| m.status == OutboxStatus::Published)
                .count())
        })
    }
}

impl TransactionalOutbox<MemoryTransaction> for MemoryDatabase {
    fn save_tx<'a>(
        &'a self,
        tx: &'a MemoryTransaction,
        message: OutboxMessage,
    ) -> BoxFuture<'a, Result<(), OutboxError>> {
        Box::pin(async move {
            tx.insert_outbox(message)
                .map_err(|e| OutboxError::Repository(e.to_string()))
        })
    }
}
