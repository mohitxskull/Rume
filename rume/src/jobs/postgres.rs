//! Production PostgreSQL-backed durable background job queue.
//!
//! Provides:
//! - Persistent job storage in PostgreSQL with ACID transactional guarantees
//! - Concurrent atomic job claiming using `FOR UPDATE SKIP LOCKED`
//! - Worker assignment, lease duration, and heartbeat expiration
//! - Dead-worker abandoned job reclamation
//! - Configurable retry policies with exponential backoff and dead-letter queue (DLQ)
//! - Idempotency deduplication

use std::fmt::Debug;
use std::time::{Duration, Instant};

use deadpool_postgres::Pool;
use tokio_postgres::types::ToSql;
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

/// Production PostgreSQL-backed implementation of [`JobQueue`].
#[derive(Clone)]
pub struct PostgresJobQueue {
    pool: Pool,
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
    pub const SCHEMA: &'static str = r#"
CREATE TABLE IF NOT EXISTS rume_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    payload BYTEA NOT NULL,
    attempt INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    initial_backoff_ms BIGINT NOT NULL DEFAULT 1000,
    max_backoff_ms BIGINT NOT NULL DEFAULT 60000,
    backoff_factor DOUBLE PRECISION NOT NULL DEFAULT 2.0,
    timeout_ms BIGINT,
    idempotency_key TEXT,
    assigned_worker TEXT,
    available_at BIGINT NOT NULL,
    lease_expires_at BIGINT,
    status TEXT NOT NULL DEFAULT 'pending',
    dead_letter_reason TEXT,
    created_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rume_jobs_pending ON rume_jobs (available_at) WHERE status = 'pending';
CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency ON rume_jobs (idempotency_key) WHERE idempotency_key IS NOT NULL AND status != 'dead_letter';
"#;

    /// Creates a new PostgreSQL job queue with a 60-second default lease duration.
    pub fn new(pool: Pool) -> Self {
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
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Initializes the `rume_jobs` table and indexes if not already present.
    pub async fn init_schema(&self) -> Result<(), JobError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| JobError::Fatal(format!("failed to connect: {e}")))?;
        client
            .batch_execute(Self::SCHEMA)
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

            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();
            let available_at = now.saturating_add(delay.as_millis() as i64);
            let max_attempts = retry_policy.max_attempts as i32;
            let initial_backoff_ms = retry_policy.initial_backoff_ms as i64;
            let max_backoff_ms = retry_policy.max_backoff_ms as i64;
            let backoff_factor = retry_policy.backoff_factor;
            let timeout_ms = timeout.map(|t| t.as_millis() as i64);

            let insert_stmt = r#"
INSERT INTO rume_jobs (
    id, name, payload, attempt, max_attempts, initial_backoff_ms,
    max_backoff_ms, backoff_factor, timeout_ms, idempotency_key,
    available_at, status, created_at
) VALUES ($1, $2, $3, 0, $4, $5, $6, $7, $8, $9, $10, 'pending', $11)
ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL AND status != 'dead_letter'
DO NOTHING
RETURNING id;
"#;

            let params: &[&(dyn ToSql + Sync)] = &[
                &id,
                &name,
                &payload,
                &max_attempts,
                &initial_backoff_ms,
                &max_backoff_ms,
                &backoff_factor,
                &timeout_ms,
                &idempotency_key,
                &available_at,
                &now,
            ];

            let rows = client
                .query(insert_stmt, params)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(row) = rows.first() {
                let inserted_id: String = row.get(0);
                Ok(inserted_id)
            } else if let Some(ref ikey) = idempotency_key {
                // Return existing job ID if deduplicated
                let existing_stmt = "SELECT id FROM rume_jobs WHERE idempotency_key = $1 AND status != 'dead_letter'";
                let existing = client
                    .query_opt(existing_stmt, &[ikey])
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;
                if let Some(erow) = existing {
                    Ok(erow.get(0))
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
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();
            let lease_duration = self.default_lease_duration.as_millis() as i64;
            let lease_expires_at = now.saturating_add(lease_duration);

            let claim_stmt = r#"
UPDATE rume_jobs
SET status = 'in_flight',
    assigned_worker = $1,
    attempt = attempt + 1,
    lease_expires_at = $2
WHERE id IN (
    SELECT id FROM rume_jobs
    WHERE (status = 'pending' AND available_at <= $3)
       OR (status = 'in_flight' AND lease_expires_at <= $3)
    ORDER BY available_at ASC, created_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, assigned_worker, available_at;
"#;

            let params: &[&(dyn ToSql + Sync)] = &[&wid, &lease_expires_at, &now];

            let rows = client
                .query(claim_stmt, params)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(row) = rows.first() {
                let id: String = row.get(0);
                let name: String = row.get(1);
                let payload: Vec<u8> = row.get(2);
                let attempt: i32 = row.get(3);
                let max_attempts: i32 = row.get(4);
                let initial_backoff_ms: i64 = row.get(5);
                let max_backoff_ms: i64 = row.get(6);
                let backoff_factor: f64 = row.get(7);
                let timeout_ms: Option<i64> = row.get(8);
                let idempotency_key: Option<String> = row.get(9);
                let assigned_worker: Option<String> = row.get(10);
                let available_at_ms: i64 = row.get(11);

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
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            client
                .execute("DELETE FROM rume_jobs WHERE id = $1", &[&jid])
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn nack(&self, job: QueuedJob, delay: Duration) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();
            let next_available = now.saturating_add(delay.as_millis() as i64);

            let stmt = r#"
UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    available_at = $1
WHERE id = $2;
"#;
            client
                .execute(stmt, &[&next_available, &job.id])
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(())
        })
    }

    fn dead_letter(&self, job: QueuedJob, reason: String) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let stmt = r#"
UPDATE rume_jobs
SET status = 'dead_letter',
    assigned_worker = NULL,
    lease_expires_at = NULL,
    dead_letter_reason = $1
WHERE id = $2;
"#;
            client
                .execute(stmt, &[&reason, &job.id])
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
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let now = current_epoch_millis();

            let stmt = r#"
UPDATE rume_jobs
SET status = 'pending',
    assigned_worker = NULL,
    lease_expires_at = NULL
WHERE status = 'in_flight' AND (assigned_worker = ANY($1) OR lease_expires_at <= $2);
"#;
            let params: &[&(dyn ToSql + Sync)] = &[&workers, &now];
            let affected = client
                .execute(stmt, params)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(affected as usize)
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let row = client
                .query_one(
                    "SELECT count(*) FROM rume_jobs WHERE status = 'pending'",
                    &[],
                )
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.get(0);
            Ok(count as usize)
        })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let row = client
                .query_one(
                    "SELECT count(*) FROM rume_jobs WHERE status = 'dead_letter'",
                    &[],
                )
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.get(0);
            Ok(count as usize)
        })
    }
}
