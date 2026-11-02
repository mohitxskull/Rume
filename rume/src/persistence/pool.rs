//! Connection pool lifecycle and management.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;
use crate::routing::{HealthCheck, HealthCheckResult, HealthStatus};

/// Metrics and status information for a database connection pool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PoolStatus {
    pub active_connections: u32,
    pub total_connections: u32,
    pub max_connections: u32,
    pub is_closed: bool,
}

/// A connection pool that manages database connection lifecycles, health checks, and transactions.
pub struct DatabasePool<B: DatabaseBackend> {
    backend: Arc<B>,
    config: DatabaseConfig,
    active_connections: Arc<AtomicU32>,
    total_connections: Arc<AtomicU32>,
    is_closed: Arc<AtomicBool>,
}

impl<B: DatabaseBackend> Clone for DatabasePool<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            config: self.config.clone(),
            active_connections: Arc::clone(&self.active_connections),
            total_connections: Arc::clone(&self.total_connections),
            is_closed: Arc::clone(&self.is_closed),
        }
    }
}

impl<B: DatabaseBackend> Debug for DatabasePool<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabasePool")
            .field("config", &self.config)
            .field("status", &self.status())
            .finish()
    }
}

impl<B: DatabaseBackend> DatabasePool<B> {
    /// Creates a new database pool with the given backend and configuration.
    pub fn new(backend: B, config: DatabaseConfig) -> Self {
        let initial_conns = config.min_connections;
        Self {
            backend: Arc::new(backend),
            config,
            active_connections: Arc::new(AtomicU32::new(0)),
            total_connections: Arc::new(AtomicU32::new(initial_conns)),
            is_closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns the pool's configuration.
    pub fn config(&self) -> &DatabaseConfig {
        &self.config
    }

    /// Returns the underlying database backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Returns the current pool status and connection metrics.
    pub fn status(&self) -> PoolStatus {
        PoolStatus {
            active_connections: self.active_connections.load(Ordering::Relaxed),
            total_connections: self.total_connections.load(Ordering::Relaxed),
            max_connections: self.config.max_connections,
            is_closed: self.is_closed.load(Ordering::Relaxed),
        }
    }

    /// Acquires a connection lease from the pool, checking capacity and closed state.
    pub fn acquire_lease(&self) -> Result<ConnectionLease, PersistenceError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(PersistenceError::Closed);
        }

        let active = self.active_connections.fetch_add(1, Ordering::SeqCst);
        if active >= self.config.max_connections {
            self.active_connections.fetch_sub(1, Ordering::SeqCst);
            return Err(PersistenceError::PoolExhausted {
                active,
                max: self.config.max_connections,
            });
        }

        Ok(ConnectionLease {
            counter: Arc::clone(&self.active_connections),
        })
    }

    /// Executes an explicit transaction boundary.
    ///
    /// The transaction is automatically committed if the closure returns `Ok(res)`.
    /// If the closure returns `Err(err)` or panics, the transaction is automatically rolled back.
    pub async fn transaction<T, E, F, Fut>(&self, f: F) -> Result<T, PersistenceError>
    where
        F: FnOnce(B::Transaction) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        PersistenceError: From<E>,
    {
        let _lease = self.acquire_lease()?;
        let tx = self.backend.begin_transaction().await?;
        let tx_handle = tx.clone();

        match f(tx).await {
            Ok(value) => {
                tx_handle.commit().await?;
                Ok(value)
            }
            Err(err) => {
                let _ = tx_handle.rollback().await;
                Err(PersistenceError::from(err))
            }
        }
    }

    /// Gracefully closes the connection pool, refusing new leases and draining connections.
    pub fn close(&self) {
        self.is_closed.store(true, Ordering::SeqCst);
        self.total_connections.store(0, Ordering::Relaxed);
    }

    /// Creates a Rume [`HealthCheck`] for this database pool.
    pub fn health_check(&self, name: impl Into<String>) -> DatabaseHealthCheck<B> {
        DatabaseHealthCheck {
            name: name.into(),
            pool: self.clone(),
        }
    }
}

/// An active connection lease that decrements the pool's active counter when dropped.
#[derive(Debug)]
pub struct ConnectionLease {
    counter: Arc<AtomicU32>,
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Health check implementation for database connection pools.
pub struct DatabaseHealthCheck<B: DatabaseBackend> {
    name: String,
    pool: DatabasePool<B>,
}

impl<B: DatabaseBackend> Clone for DatabaseHealthCheck<B> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<B: DatabaseBackend> Debug for DatabaseHealthCheck<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseHealthCheck")
            .field("name", &self.name)
            .field("status", &self.pool.status())
            .finish()
    }
}

impl<B: DatabaseBackend> HealthCheck for DatabaseHealthCheck<B> {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self) -> Pin<Box<dyn std::future::Future<Output = HealthCheckResult> + Send>> {
        let name = self.name.clone();
        let pool = self.pool.clone();

        Box::pin(async move {
            let status = pool.status();
            let mut meta = BTreeMap::new();
            meta.insert(
                "active_connections".to_string(),
                serde_json::json!(status.active_connections),
            );
            meta.insert(
                "total_connections".to_string(),
                serde_json::json!(status.total_connections),
            );
            meta.insert(
                "max_connections".to_string(),
                serde_json::json!(status.max_connections),
            );

            if status.is_closed {
                return HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: "database pool is closed".to_string(),
                    meta,
                    cached: false,
                };
            }

            match pool.backend().ping().await {
                Ok(()) => HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "database connection healthy".to_string(),
                    meta,
                    cached: false,
                },
                Err(err) => HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!("database connection check failed: {err}"),
                    meta,
                    cached: false,
                },
            }
        })
    }
}
