//! Read/write replica pool routing with health-aware round-robin and RYOW fallback.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rume_foundation::health::{HealthCheck, HealthCheckResult, HealthStatus};

use crate::persistence::backend::{DatabaseBackend, DbValue};
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::{DatabasePool, PoolStatus};
use crate::persistence::row::{DbRow, FromRow};

/// Configuration for [`ReadWritePool`] read routing and read-your-own-writes behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadWriteConfig {
    /// Duration after a write during which reads route to the primary pool to ensure
    /// read-your-own-writes (RYOW) consistency.
    pub ryow_window: Duration,
    /// Whether closed/unhealthy replicas should be skipped during round-robin selection.
    pub skip_unhealthy_replicas: bool,
}

impl Default for ReadWriteConfig {
    fn default() -> Self {
        Self {
            ryow_window: Duration::from_secs(2),
            skip_unhealthy_replicas: true,
        }
    }
}

impl ReadWriteConfig {
    /// Sets the read-your-own-writes fallback window.
    pub fn with_ryow_window(mut self, ryow_window: Duration) -> Self {
        self.ryow_window = ryow_window;
        self
    }

    /// Sets whether closed/unhealthy replicas are skipped during read routing.
    pub fn with_skip_unhealthy_replicas(mut self, skip_unhealthy_replicas: bool) -> Self {
        self.skip_unhealthy_replicas = skip_unhealthy_replicas;
        self
    }
}

/// Snapshot of primary and replica pool statuses for a [`ReadWritePool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadWritePoolStatus {
    /// Status of the primary (read/write) pool.
    pub primary: PoolStatus,
    /// Status of each configured replica pool in registration order.
    pub replicas: Vec<PoolStatus>,
    /// Number of replicas that are currently open/healthy.
    pub healthy_replicas: usize,
    /// Whether reads are currently pinned to primary due to a recent write.
    pub within_ryow_window: bool,
}

/// Connection pool router that sends writes to a primary pool and round-robins reads
/// across healthy replica pools, falling back to primary when no healthy replicas are
/// available or within the configured read-your-own-writes (`ryow_window`) period.
#[derive(Debug)]
pub struct ReadWritePool<B: DatabaseBackend> {
    primary: DatabasePool<B>,
    replicas: Vec<DatabasePool<B>>,
    config: ReadWriteConfig,
    next_replica: Arc<AtomicUsize>,
    created_at: Instant,
    // Why: 0 encodes "no write recorded yet" while any recorded write stores
    // `elapsed_nanos + 1`. This avoids an `Instant::now()` colliding with 0ns at pool
    // creation and keeps `fetch_max` lock-free and monotonically non-decreasing.
    last_write_nanos_plus_one: Arc<AtomicU64>,
}

impl<B: DatabaseBackend> Clone for ReadWritePool<B> {
    fn clone(&self) -> Self {
        Self {
            primary: self.primary.clone(),
            replicas: self.replicas.clone(),
            config: self.config.clone(),
            next_replica: Arc::clone(&self.next_replica),
            created_at: self.created_at,
            last_write_nanos_plus_one: Arc::clone(&self.last_write_nanos_plus_one),
        }
    }
}

impl<B: DatabaseBackend> ReadWritePool<B> {
    /// Creates a new [`ReadWritePool`] with a primary pool, zero or more replicas, and config.
    pub fn new(
        primary: DatabasePool<B>,
        replicas: Vec<DatabasePool<B>>,
        config: ReadWriteConfig,
    ) -> Self {
        Self {
            primary,
            replicas,
            config,
            next_replica: Arc::new(AtomicUsize::new(0)),
            created_at: Instant::now(),
            last_write_nanos_plus_one: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns a reference to the primary pool.
    pub fn primary(&self) -> &DatabasePool<B> {
        &self.primary
    }

    /// Returns the configured replica pools.
    pub fn replicas(&self) -> &[DatabasePool<B>] {
        &self.replicas
    }

    /// Returns the pool routing configuration.
    pub fn config(&self) -> &ReadWriteConfig {
        &self.config
    }

    /// Records that a write occurred now, pinning subsequent reads to the primary pool
    /// for [`ReadWriteConfig::ryow_window`].
    pub fn record_write(&self) {
        let elapsed_ns = self
            .created_at
            .elapsed()
            .as_nanos()
            .min(u64::MAX as u128 - 1) as u64;
        self.last_write_nanos_plus_one
            .fetch_max(elapsed_ns.saturating_add(1), Ordering::Release);
    }

    /// Returns `true` if a write was recorded within [`ReadWriteConfig::ryow_window`].
    pub fn is_within_ryow_window(&self) -> bool {
        if self.config.ryow_window.is_zero() {
            return false;
        }
        let last = self.last_write_nanos_plus_one.load(Ordering::Acquire);
        if last == 0 {
            return false;
        }
        let last_elapsed_ns = last - 1;
        let now_elapsed_ns = self.created_at.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let since_write = Duration::from_nanos(now_elapsed_ns.saturating_sub(last_elapsed_ns));
        since_write < self.config.ryow_window
    }

    /// Selects a replica pool via round-robin (skipping closed replicas when configured),
    /// or falls back to the primary pool if no healthy replica is available.
    pub fn replica_or_primary(&self) -> &DatabasePool<B> {
        let len = self.replicas.len();
        if len == 0 {
            return &self.primary;
        }

        let start = self.next_replica.fetch_add(1, Ordering::Relaxed) % len;
        if !self.config.skip_unhealthy_replicas {
            return &self.replicas[start];
        }

        for offset in 0..len {
            let idx = (start + offset) % len;
            let candidate = &self.replicas[idx];
            if !candidate.status().is_closed {
                return candidate;
            }
        }

        &self.primary
    }

    /// Selects the pool to use for a read operation: primary if no replicas exist,
    /// all replicas are closed/unhealthy, or a write occurred within `ryow_window`;
    /// otherwise round-robins across healthy replicas.
    pub fn read_pool(&self) -> &DatabasePool<B> {
        if self.replicas.is_empty() || self.is_within_ryow_window() {
            return &self.primary;
        }
        self.replica_or_primary()
    }

    /// Executes a SQL or DDL statement on the primary pool and records a write.
    pub async fn execute(&self, statement: &str) -> Result<u64, PersistenceError> {
        let affected = self.primary.execute(statement).await?;
        self.record_write();
        Ok(affected)
    }

    /// Executes a parameterized SQL statement on the primary pool and records a write.
    pub async fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<u64, PersistenceError> {
        let affected = self.primary.execute_params(statement, params).await?;
        self.record_write();
        Ok(affected)
    }

    /// Executes an explicit transaction on the primary pool and records a write on commit.
    pub async fn transaction<T, E, F, Fut>(&self, f: F) -> Result<T, PersistenceError>
    where
        F: FnOnce(B::Transaction) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        PersistenceError: From<E>,
    {
        let value = self.primary.transaction(f).await?;
        self.record_write();
        Ok(value)
    }

    /// Fetches all matching rows as generic [`DbRow`] records from [`Self::read_pool`].
    pub async fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<Vec<DbRow>, PersistenceError> {
        self.read_pool().fetch_all(statement, params).await
    }

    /// Fetches all matching rows from [`Self::read_pool`] and maps them into `R` via [`FromRow`].
    pub async fn fetch_all_as<R: FromRow>(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<Vec<R>, PersistenceError> {
        self.read_pool().fetch_all_as(statement, params).await
    }

    /// Fetches an optional single row as [`DbRow`] from [`Self::read_pool`].
    pub async fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<Option<DbRow>, PersistenceError> {
        self.read_pool().fetch_optional(statement, params).await
    }

    /// Fetches an optional single row from [`Self::read_pool`] and maps it into `R` via [`FromRow`].
    pub async fn fetch_optional_as<R: FromRow>(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<Option<R>, PersistenceError> {
        self.read_pool().fetch_optional_as(statement, params).await
    }

    /// Fetches exactly one row as [`DbRow`] from [`Self::read_pool`].
    pub async fn fetch_one(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<DbRow, PersistenceError> {
        self.read_pool().fetch_one(statement, params).await
    }

    /// Fetches exactly one row from [`Self::read_pool`] and maps it into `R` via [`FromRow`].
    pub async fn fetch_one_as<R: FromRow>(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> Result<R, PersistenceError> {
        self.read_pool().fetch_one_as(statement, params).await
    }

    /// Returns a combined status snapshot for the primary and all replica pools.
    pub fn status(&self) -> ReadWritePoolStatus {
        let primary = self.primary.status();
        let replicas: Vec<PoolStatus> = self.replicas.iter().map(DatabasePool::status).collect();
        let healthy_replicas = replicas.iter().filter(|s| !s.is_closed).count();
        ReadWritePoolStatus {
            primary,
            replicas,
            healthy_replicas,
            within_ryow_window: self.is_within_ryow_window(),
        }
    }

    /// Creates a Rume [`HealthCheck`] reporting primary and replica health.
    pub fn health_check(&self, name: impl Into<String>) -> ReadWriteHealthCheck<B> {
        ReadWriteHealthCheck {
            name: name.into(),
            pool: self.clone(),
        }
    }
}

/// Health check implementation for [`ReadWritePool`].
#[derive(Debug)]
pub struct ReadWriteHealthCheck<B: DatabaseBackend> {
    name: String,
    pool: ReadWritePool<B>,
}

impl<B: DatabaseBackend> Clone for ReadWriteHealthCheck<B> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<B: DatabaseBackend> HealthCheck for ReadWriteHealthCheck<B> {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
        let name = self.name.clone();
        let pool = self.pool.clone();

        Box::pin(async move {
            let primary_res = pool
                .primary()
                .health_check(format!("{name}-primary"))
                .run()
                .await;

            let mut unhealthy_replicas = 0usize;
            for (idx, replica) in pool.replicas().iter().enumerate() {
                let res = replica
                    .health_check(format!("{name}-replica-{idx}"))
                    .run()
                    .await;
                if res.status != HealthStatus::Ok {
                    unhealthy_replicas += 1;
                }
            }

            let replica_count = pool.replicas().len();
            let healthy_replicas = replica_count.saturating_sub(unhealthy_replicas);

            let mut meta = BTreeMap::new();
            meta.insert(
                "primary_status".to_string(),
                serde_json::json!(primary_res.status),
            );
            meta.insert(
                "replica_count".to_string(),
                serde_json::json!(replica_count),
            );
            meta.insert(
                "healthy_replicas".to_string(),
                serde_json::json!(healthy_replicas),
            );
            meta.insert(
                "within_ryow_window".to_string(),
                serde_json::json!(pool.is_within_ryow_window()),
            );

            if primary_res.status == HealthStatus::Failed {
                return HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!("primary pool unhealthy: {}", primary_res.message),
                    meta,
                    cached: false,
                    duration_ms: None,
                };
            }

            if unhealthy_replicas > 0 {
                return HealthCheckResult {
                    name,
                    status: HealthStatus::Warning,
                    message: format!(
                        "{unhealthy_replicas} of {replica_count} replica pools unhealthy; primary healthy"
                    ),
                    meta,
                    cached: false,
                    duration_ms: None,
                };
            }

            HealthCheckResult {
                name,
                status: HealthStatus::Ok,
                message: format!("primary and all {replica_count} replica pools healthy"),
                meta,
                cached: false,
                duration_ms: None,
            }
        })
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::persistence::{DatabaseConfig, DatabasePool, SqliteBackend};

    #[derive(Debug, PartialEq, Eq)]
    struct TagRow(String);

    impl FromRow for TagRow {
        fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
            row.get::<String>("name").map(Self)
        }
    }

    #[tokio::test]
    async fn read_write_pool_round_robins_across_healthy_replicas_and_skips_closed() {
        let primary = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("primary"),
            DatabaseConfig::new("sqlite::memory:"),
        );
        let r1 = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("r1"),
            DatabaseConfig::new("sqlite::memory:"),
        );
        let r2 = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("r2"),
            DatabaseConfig::new("sqlite::memory:"),
        );
        let r3 = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("r3"),
            DatabaseConfig::new("sqlite::memory:"),
        );

        primary
            .execute("CREATE TABLE tag (name TEXT NOT NULL); INSERT INTO tag VALUES ('primary');")
            .await
            .unwrap();
        r1.execute("CREATE TABLE tag (name TEXT NOT NULL); INSERT INTO tag VALUES ('r1');")
            .await
            .unwrap();
        r2.execute("CREATE TABLE tag (name TEXT NOT NULL); INSERT INTO tag VALUES ('r2');")
            .await
            .unwrap();
        r3.execute("CREATE TABLE tag (name TEXT NOT NULL); INSERT INTO tag VALUES ('r3');")
            .await
            .unwrap();

        let pool = ReadWritePool::new(
            primary,
            vec![r1.clone(), r2.clone(), r3.clone()],
            ReadWriteConfig::default(),
        );

        let mut seen = Vec::new();
        for _ in 0..6 {
            let tag = pool
                .fetch_one_as::<TagRow>("SELECT name FROM tag LIMIT 1", &[])
                .await
                .unwrap();
            seen.push(tag.0);
        }
        assert_eq!(seen, vec!["r1", "r2", "r3", "r1", "r2", "r3"]);

        // Close r2 and verify subsequent reads skip it.
        r2.close_async().await;
        let mut seen_after_close = Vec::new();
        for _ in 0..4 {
            let tag = pool
                .fetch_one_as::<TagRow>("SELECT name FROM tag LIMIT 1", &[])
                .await
                .unwrap();
            seen_after_close.push(tag.0);
        }
        assert!(!seen_after_close.iter().any(|t| t == "r2"));
        assert!(seen_after_close.iter().any(|t| t == "r1"));
        assert!(seen_after_close.iter().any(|t| t == "r3"));

        // Close remaining replicas and verify fallback to primary.
        r1.close_async().await;
        r3.close_async().await;
        let fallback = pool
            .fetch_one_as::<TagRow>("SELECT name FROM tag LIMIT 1", &[])
            .await
            .unwrap();
        assert_eq!(fallback.0, "primary");
    }

    #[tokio::test]
    async fn read_write_pool_falls_back_to_primary_when_no_replicas() {
        let primary = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("primary"),
            DatabaseConfig::new("sqlite::memory:"),
        );
        primary
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .await
            .unwrap();

        let pool = ReadWritePool::new(primary, vec![], ReadWriteConfig::default());
        assert!(pool.replicas().is_empty());

        pool.execute("INSERT INTO items (id, name) VALUES (1, 'alpha')")
            .await
            .unwrap();
        let row = pool
            .fetch_one("SELECT name FROM items WHERE id = 1", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<String>("name").unwrap(), "alpha");
    }

    #[tokio::test]
    async fn read_write_pool_honors_ryow_window_after_write() {
        let primary = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("primary"),
            DatabaseConfig::new("sqlite::memory:"),
        );
        let replica = DatabasePool::new(
            SqliteBackend::in_memory().await.expect("replica"),
            DatabaseConfig::new("sqlite::memory:"),
        );

        primary
            .execute("CREATE TABLE tag (name TEXT NOT NULL); INSERT INTO tag VALUES ('primary');")
            .await
            .unwrap();
        replica
            .execute("CREATE TABLE tag (name TEXT NOT NULL); INSERT INTO tag VALUES ('replica');")
            .await
            .unwrap();

        let config = ReadWriteConfig {
            ryow_window: Duration::from_millis(80),
            skip_unhealthy_replicas: true,
        };
        let pool = ReadWritePool::new(primary, vec![replica], config);

        // Before any write, reads go to replica.
        let before = pool
            .fetch_one_as::<TagRow>("SELECT name FROM tag LIMIT 1", &[])
            .await
            .unwrap();
        assert_eq!(before.0, "replica");

        // After a write, reads within ryow_window route to primary.
        pool.execute("CREATE TABLE IF NOT EXISTS dummy (id INTEGER)")
            .await
            .unwrap();
        let during = pool
            .fetch_one_as::<TagRow>("SELECT name FROM tag LIMIT 1", &[])
            .await
            .unwrap();
        assert_eq!(during.0, "primary");

        // Once ryow_window elapses, reads return to replica.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = pool
            .fetch_one_as::<TagRow>("SELECT name FROM tag LIMIT 1", &[])
            .await
            .unwrap();
        assert_eq!(after.0, "replica");
    }
}
