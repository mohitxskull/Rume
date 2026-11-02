//! Production PostgreSQL-backed durable background job queue using SQLx.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob};
use crate::jobs::queue::JobQueue;
use crate::jobs::retry::RetryPolicy;
use crate::persistence::postgres::PostgresBackend;

fn current_epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

const JOBS_SCHEMA: &str = include_str!("../persistence/sql/postgres/jobs_schema.sql");
const JOBS_CLAIM_SQL: &str = include_str!("../persistence/sql/postgres/jobs_claim.sql");
const INSERT_JOB_SQL: &str = include_str!("../persistence/sql/postgres/jobs_insert.sql");
const JOBS_REAP_SQL: &str = include_str!("../persistence/sql/postgres/jobs_reap.sql");

/// Production PostgreSQL-backed implementation of [`JobQueue`].
#[derive(Clone)]
pub struct PostgresJobQueue {
    pool: PgPool,
    default_lease_duration: Duration,
}

impl Debug for PostgresJobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresJobQueue")
            .field("default_lease_duration", &self.default_lease_duration)
            .finish()
    }
}

impl PostgresJobQueue {
    /// Schema DDL for durable background jobs.
    pub const SCHEMA: &'static str = JOBS_SCHEMA;

    /// Creates a new PostgreSQL job queue with a 60-second default lease duration.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            default_lease_duration: Duration::from_secs(60),
        }
    }

    /// Creates a job queue from a [`PostgresBackend`].
    pub fn from_backend(backend: &PostgresBackend) -> Self {
        Self::new(backend.pool().clone())
    }

    /// Sets the worker claim lease duration.
    pub fn with_lease_duration(mut self, duration: Duration) -> Self {
        self.default_lease_duration = duration;
        self
    }

    /// Access the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Initializes the `rume_jobs` table and indexes if not already present.
    pub async fn init_schema(&self) -> Result<(), JobError> {
        sqlx::raw_sql(Self::SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(format!("failed to init schema: {e}")))?;
        Ok(())
    }
}

impl JobQueue for PostgresJobQueue {
    fn enqueue_delayed<J: Job>(
        &self,
        job: J,
        delay: Duration,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        let name = J::NAME.to_string();
        let retry_policy = job.retry_policy();
        let timeout = job.timeout();
        let idempotency_key = job.idempotency_key();
        let id = Uuid::new_v4().to_string();

        Box::pin(async move {
            let payload =
                serde_json::to_vec(&job).map_err(|e| JobError::Serialization(e.to_string()))?;

            let now = current_epoch_millis();
            let available_at = now.saturating_add(delay.as_millis() as i64);
            let max_attempts = retry_policy.max_attempts as i32;
            let initial_backoff_ms = retry_policy.initial_backoff_ms as i64;
            let max_backoff_ms = retry_policy.max_backoff_ms as i64;
            let backoff_factor = retry_policy.backoff_factor;
            let timeout_ms = timeout.map(|t| t.as_millis() as i64);

            let row = sqlx::query(INSERT_JOB_SQL)
                .bind(&id)
                .bind(&name)
                .bind(&payload)
                .bind(max_attempts)
                .bind(initial_backoff_ms)
                .bind(max_backoff_ms)
                .bind(backoff_factor)
                .bind(timeout_ms)
                .bind(&idempotency_key)
                .bind(available_at)
                .bind(now)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(r) = row {
                let inserted_id: String = r.try_get(0).unwrap_or(id);
                Ok(inserted_id)
            } else if let Some(ref ikey) = idempotency_key {
                let existing = sqlx::query(
                    "SELECT id FROM rume_jobs WHERE idempotency_key = $1 AND status != 'dead_letter'"
                )
                .bind(ikey)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

                if let Some(erow) = existing {
                    Ok(erow.try_get(0).unwrap_or(id))
                } else {
                    Ok(id)
                }
            } else {
                Ok(id)
            }
        })
    }

    fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        self.dequeue_for("default-worker")
    }

    fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            let now = current_epoch_millis();
            let lease_duration = self.default_lease_duration.as_millis() as i64;
            let lease_expires_at = now.saturating_add(lease_duration);

            let row: Option<PgRow> = sqlx::query(JOBS_CLAIM_SQL)
                .bind(&wid)
                .bind(lease_expires_at)
                .bind(now)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(r) = row {
                let id: String = r.try_get(0).map_err(|e| JobError::Fatal(e.to_string()))?;
                let name: String = r.try_get(1).map_err(|e| JobError::Fatal(e.to_string()))?;
                let payload: Vec<u8> = r.try_get(2).map_err(|e| JobError::Fatal(e.to_string()))?;
                let attempt: i32 = r.try_get(3).unwrap_or(0);
                let max_attempts: i32 = r.try_get(4).unwrap_or(3);
                let initial_backoff_ms: i64 = r.try_get(5).unwrap_or(1000);
                let max_backoff_ms: i64 = r.try_get(6).unwrap_or(60000);
                let backoff_factor: f64 = r.try_get(7).unwrap_or(2.0);
                let timeout_ms: Option<i64> = r.try_get(8).ok();
                let idempotency_key: Option<String> = r.try_get(9).ok();
                let assigned_worker: Option<String> = r.try_get(10).ok();
                let available_at_ms: i64 = r.try_get(11).unwrap_or(now);

                let retry_policy = RetryPolicy {
                    max_attempts: max_attempts.max(1) as u32,
                    initial_backoff_ms: initial_backoff_ms.max(0) as u64,
                    max_backoff_ms: max_backoff_ms.max(0) as u64,
                    backoff_factor,
                };

                let timeout = timeout_ms.map(|ms| Duration::from_millis(ms.max(0) as u64));

                let available_at = if available_at_ms <= now {
                    Instant::now() - Duration::from_millis((now - available_at_ms).max(0) as u64)
                } else {
                    Instant::now() + Duration::from_millis((available_at_ms - now).max(0) as u64)
                };

                Ok(Some(QueuedJob {
                    id,
                    name,
                    payload,
                    attempt: attempt.max(1) as u32,
                    retry_policy,
                    timeout,
                    idempotency_key,
                    assigned_worker,
                    available_at,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn ack(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        Box::pin(async move {
            sqlx::query("DELETE FROM rume_jobs WHERE id = $1")
                .bind(&jid)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            Ok(())
        })
    }

    fn nack(&self, job: QueuedJob, delay: Duration) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let now = current_epoch_millis();
            let next_available = now.saturating_add(delay.as_millis() as i64);

            sqlx::query(
                "UPDATE rume_jobs SET status = 'pending', assigned_worker = NULL, lease_expires_at = NULL, available_at = $1 WHERE id = $2"
            )
            .bind(next_available)
            .bind(&job.id)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn dead_letter(&self, job: QueuedJob, reason: String) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            sqlx::query(
                "UPDATE rume_jobs SET status = 'dead_letter', assigned_worker = NULL, lease_expires_at = NULL, dead_letter_reason = $1 WHERE id = $2"
            )
            .bind(&reason)
            .bind(&job.id)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn reclaim_abandoned_jobs(
        &self,
        dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>> {
        let workers = dead_workers.to_vec();
        Box::pin(async move {
            let now = current_epoch_millis();
            let res = sqlx::query(JOBS_REAP_SQL)
                .bind(&workers)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(res.rows_affected() as usize)
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT count(*) FROM rume_jobs WHERE status = 'pending'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT count(*) FROM rume_jobs WHERE status = 'dead_letter'")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }
}
