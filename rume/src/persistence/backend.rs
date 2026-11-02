//! Database backend abstraction and in-memory test implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

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
    tables: BTreeSet<String>,
    applied_migrations: BTreeSet<String>,
    alive: bool,
}

/// An in-memory database backend providing zero-dependency persistence for tests and prototyping.
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
                tables: BTreeSet::new(),
                applied_migrations: BTreeSet::new(),
                alive: true,
            })),
        }
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
        let stmt = statement.trim().to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            if !state.alive {
                return Err(PersistenceError::Closed);
            }

            if stmt.starts_with("CREATE TABLE") {
                let name = stmt
                    .strip_prefix("CREATE TABLE")
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("unknown");
                state.tables.insert(name.to_string());
                Ok(1)
            } else if stmt.starts_with("SET ") {
                let rest = stmt.strip_prefix("SET ").unwrap_or("");
                if let Some((k, v)) = rest.split_once('=') {
                    state
                        .kv_store
                        .insert(k.trim().to_string(), v.trim().to_string());
                    Ok(1)
                } else {
                    Err(PersistenceError::QueryFailed(
                        "invalid SET statement".into(),
                    ))
                }
            } else if stmt.starts_with("DELETE ") {
                let rest = stmt.strip_prefix("DELETE ").unwrap_or("");
                state.kv_store.remove(rest.trim());
                Ok(1)
            } else {
                Ok(1)
            }
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

            if q.starts_with("GET ") {
                let key = q.strip_prefix("GET ").unwrap_or("").trim();
                Ok(state.kv_store.get(key).cloned())
            } else if q.starts_with("EXISTS TABLE ") {
                let tbl = q.strip_prefix("EXISTS TABLE ").unwrap_or("").trim();
                Ok(Some(state.tables.contains(tbl).to_string()))
            } else {
                Ok(None)
            }
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
    tables: BTreeSet<String>,
}

/// Active transaction for the in-memory database.
#[derive(Debug, Clone)]
pub struct MemoryTransaction {
    state: Arc<RwLock<MemoryDbState>>,
    staged: Arc<RwLock<MemoryStagedState>>,
    status: Arc<AtomicU8>, // 0: Active, 1: Committed, 2: RolledBack
}

impl DatabaseTransaction for MemoryTransaction {
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.trim().to_string();
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

            if stmt.starts_with("CREATE TABLE") {
                let name = stmt
                    .strip_prefix("CREATE TABLE")
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("unknown");
                staged.tables.insert(name.to_string());
                Ok(1)
            } else if stmt.starts_with("SET ") {
                let rest = stmt.strip_prefix("SET ").unwrap_or("");
                if let Some((k, v)) = rest.split_once('=') {
                    staged
                        .kv
                        .insert(k.trim().to_string(), Some(v.trim().to_string()));
                    Ok(1)
                } else {
                    Err(PersistenceError::QueryFailed(
                        "invalid SET statement".into(),
                    ))
                }
            } else if stmt.starts_with("DELETE ") {
                let rest = stmt.strip_prefix("DELETE ").unwrap_or("");
                staged.kv.insert(rest.trim().to_string(), None);
                Ok(1)
            } else {
                Ok(1)
            }
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

            if q.starts_with("GET ") {
                let key = q.strip_prefix("GET ").unwrap_or("").trim();
                let staged = self
                    .staged
                    .read()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                if let Some(val_opt) = staged.kv.get(key) {
                    return Ok(val_opt.clone());
                }
                let state = self
                    .state
                    .read()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                Ok(state.kv_store.get(key).cloned())
            } else {
                Ok(None)
            }
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
            for tbl in std::mem::take(&mut staged.tables) {
                state.tables.insert(tbl);
            }
            Ok(())
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            if let Ok(mut staged) = self.staged.write() {
                staged.kv.clear();
                staged.tables.clear();
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
                staged.tables.clear();
            }
            let _ = self
                .status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst);
        }
    }
}
