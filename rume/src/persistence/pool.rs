//! Connection pool lifecycle and management.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, TransactionalOutbox,
};
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
    pub in_flight_transactions: u32,
    pub is_closed: bool,
}

/// A connection pool that manages database connection lifecycles, health checks, and transactions.
pub struct DatabasePool<B: DatabaseBackend> {
    backend: Arc<B>,
    config: DatabaseConfig,
    active_connections: Arc<AtomicU32>,
    total_connections: Arc<AtomicU32>,
    in_flight_transactions: Arc<AtomicU32>,
    semaphore: Arc<Semaphore>,
    is_closed: Arc<AtomicBool>,
}

impl<B: DatabaseBackend> Clone for DatabasePool<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            config: self.config.clone(),
            active_connections: Arc::clone(&self.active_connections),
            total_connections: Arc::clone(&self.total_connections),
            in_flight_transactions: Arc::clone(&self.in_flight_transactions),
            semaphore: Arc::clone(&self.semaphore),
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
        let max_conns = config.max_connections.max(1) as usize;
        Self {
            backend: Arc::new(backend),
            config,
            active_connections: Arc::new(AtomicU32::new(0)),
            total_connections: Arc::new(AtomicU32::new(initial_conns)),
            in_flight_transactions: Arc::new(AtomicU32::new(0)),
            semaphore: Arc::new(Semaphore::new(max_conns)),
            is_closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns the pool's configuration.
    pub fn config(&self) -> &DatabaseConfig {
        &self.config
    }

    /// Returns whether `auto_migrate` is enabled in configuration.
    pub fn auto_migrate(&self) -> bool {
        self.config.auto_migrate
    }

    /// Automatically applies pending migrations on the backend if `auto_migrate` is configured.
    /// Returns `Some(summary)` if migrations were executed, or `None` if `auto_migrate` was false.
    pub async fn apply_auto_migrations(
        &self,
        migrator: &crate::persistence::migration::Migrator,
    ) -> Result<Option<crate::persistence::migration::MigrationSummary>, PersistenceError> {
        if !self.config.auto_migrate {
            return Ok(None);
        }
        let summary = migrator.run_pending_backend(self.backend()).await?;
        Ok(Some(summary))
    }

    /// Returns the underlying database backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Returns the current pool status and connection metrics derived from driver state.
    pub fn status(&self) -> PoolStatus {
        let is_closed = self.is_closed.load(Ordering::Relaxed) || self.backend.is_closed();
        let total = self.backend.pool_size();
        self.total_connections.store(total, Ordering::Relaxed);
        let active = self.active_connections.load(Ordering::Relaxed);
        PoolStatus {
            active_connections: active,
            total_connections: total,
            max_connections: self.config.max_connections,
            in_flight_transactions: self.in_flight_transactions.load(Ordering::Relaxed),
            is_closed,
        }
    }

    /// Acquires a connection lease from the pool synchronously, checking capacity and closed state.
    pub fn acquire_lease(&self) -> Result<ConnectionLease, PersistenceError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(PersistenceError::Closed);
        }

        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                let active = self.active_connections.load(Ordering::SeqCst);
                return Err(PersistenceError::PoolExhausted {
                    active,
                    max: self.config.max_connections,
                });
            }
        };

        self.active_connections.fetch_add(1, Ordering::SeqCst);
        Ok(ConnectionLease {
            counter: Arc::clone(&self.active_connections),
            _permit: Some(permit),
        })
    }

    /// Acquires a connection lease asynchronously, waiting up to `timeout` if pool capacity is exhausted.
    pub async fn acquire_lease_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ConnectionLease, PersistenceError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(PersistenceError::Closed);
        }

        let permit =
            match tokio::time::timeout(timeout, self.semaphore.clone().acquire_owned()).await {
                Ok(Ok(p)) => p,
                Ok(Err(_)) => return Err(PersistenceError::Closed),
                Err(_) => return Err(PersistenceError::Timeout),
            };

        self.active_connections.fetch_add(1, Ordering::SeqCst);
        Ok(ConnectionLease {
            counter: Arc::clone(&self.active_connections),
            _permit: Some(permit),
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
        let _lease = self
            .acquire_lease_timeout(self.config.connect_timeout())
            .await?;
        self.in_flight_transactions.fetch_add(1, Ordering::SeqCst);

        struct TxGuard(Arc<AtomicU32>);
        impl Drop for TxGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _guard = TxGuard(Arc::clone(&self.in_flight_transactions));

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
        self.semaphore.close();
        let backend = Arc::clone(&self.backend);
        tokio::spawn(async move {
            backend.close().await;
        });
    }

    /// Asynchronously closes the connection pool, refusing new leases and awaiting backend driver connection teardown.
    pub async fn close_async(&self) {
        self.is_closed.store(true, Ordering::SeqCst);
        self.semaphore.close();
        self.backend.close().await;
    }

    /// Closes the pool and waits up to `timeout` for all active connections and in-flight transactions to drain.
    ///
    /// Returns `true` if cleanly drained within `timeout`, or `false` if active work remained.
    pub async fn close_and_drain(&self, timeout: Duration) -> bool {
        self.is_closed.store(true, Ordering::SeqCst);
        self.semaphore.close();
        let start = tokio::time::Instant::now();
        while self.active_connections.load(Ordering::SeqCst) > 0
            || self.in_flight_transactions.load(Ordering::SeqCst) > 0
        {
            if start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        if tokio::time::timeout(remaining, self.backend.close()).await.is_err() {
            return false;
        }

        true
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
    _permit: Option<OwnedSemaphorePermit>,
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

            let ping_timeout = pool.config().connect_timeout();
            let ping_res = tokio::time::timeout(ping_timeout, pool.backend().ping()).await;
            match ping_res {
                Ok(Ok(())) => HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "database connection healthy".to_string(),
                    meta,
                    cached: false,
                },
                Ok(Err(err)) => HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!("database connection check failed: {err}"),
                    meta,
                    cached: false,
                },
                Err(_) => HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!("database connection check timed out after {}s", ping_timeout.as_secs()),
                    meta,
                    cached: false,
                },
            }
        })
    }
}

impl<B> OutboxRepository for DatabasePool<B>
where
    B: DatabaseBackend + OutboxRepository,
{
    fn save(&self, msg: OutboxMessage) -> OutboxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().save(msg).await
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().fetch_pending(limit).await
        })
    }

    fn claim_pending(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().claim_pending(limit).await
        })
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .claim_pending_with_lease(limit, lease_duration_secs)
                .await
        })
    }

    fn renew_outbox_lease(
        &self,
        id: &str,
        lease_token: &str,
        extension_secs: u64,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        let lease_token = lease_token.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .renew_outbox_lease(&id, &lease_token, extension_secs)
                .await
        })
    }

    fn mark_published(&self, id: &str, lease_token: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        let lease_token = lease_token.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().mark_published(&id, &lease_token).await
        })
    }

    fn mark_failed(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        self.mark_failed_with_max_retries(id, lease_token, error, 5)
    }

    fn mark_failed_with_max_retries(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
        max_retries: u32,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        let lease_token = lease_token.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend()
                .mark_failed_with_max_retries(&id, &lease_token, error, max_retries)
                .await
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().recover_expired_claims(max_lease_age_secs).await
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().pending_count().await
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().published_count().await
        })
    }

    fn failed_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().failed_count().await
        })
    }

    fn fetch_failed(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().fetch_failed(limit).await
        })
    }

    fn reprocess_failed(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id = id.to_string();
        Box::pin(async move {
            let _lease = self
                .acquire_lease()
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            self.backend().reprocess_failed(&id).await
        })
    }
}

impl<B> TransactionalOutbox<B::Transaction> for DatabasePool<B>
where
    B: DatabaseBackend + TransactionalOutbox<B::Transaction>,
{
    fn save_tx<'a>(
        &'a self,
        tx: &'a B::Transaction,
        message: OutboxMessage,
    ) -> OutboxFuture<'a, Result<(), OutboxError>> {
        self.backend().save_tx(tx, message)
    }
}
