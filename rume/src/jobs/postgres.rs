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
        let trace = job.trace_context();
        self.enqueue_delayed_with_trace(job, delay, trace)
    }

    fn enqueue_delayed_with_trace<J: Job>(
        &self,
        job: J,
        delay: Duration,
        trace_context: Option<String>,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        let name = J::NAME.to_string();
        let retry_policy = job.retry_policy();
        let timeout = job.timeout();
        let idempotency_key = job.idempotency_key();
        let tenant_id = job.tenant_id();
        let id = Uuid::new_v4().to_string();

        Box::pin(async move {
            let payload =
                serde_json::to_vec(&job).map_err(|e| JobError::Serialization(e.to_string()))?;

            let delay_ms = delay.as_millis() as i64;
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
                .bind(&tenant_id)
                .bind(delay_ms)
                .bind(&trace_context)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(r) = row {
                let inserted_id: String = r.try_get(0).unwrap_or(id);
                Ok(inserted_id)
            } else if let Some(ref ikey) = idempotency_key {
                let existing = sqlx::query(
                    "SELECT id FROM rume_jobs WHERE COALESCE(tenant_id, '') = COALESCE($1, '') AND idempotency_key = $2 AND status NOT IN ('dead_letter', 'purged')"
                )
                .bind(&tenant_id)
                .bind(ikey)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

                if let Some(erow) = existing {
                    let existing_id: String = erow.try_get(0).map_err(|e| JobError::Fatal(e.to_string()))?;
                    Ok(existing_id)
                } else {
                    Err(JobError::Fatal("idempotency conflict race: conflicting row is no longer active".into()))
                }
            } else {
                Err(JobError::Fatal("conflict detected but no idempotency key was supplied".into()))
            }
        })
    }

    fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        self.dequeue_for("default-worker")
    }

    fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        let wid = worker_id.to_string();
        Box::pin(async move {
            // Use database-evaluated time: lease_expires_at = clock_timestamp() + $2 ms
            // No host-clock values are bound; the DB is the sole clock authority.
            let lease_duration_ms = self.default_lease_duration.as_millis() as i64;
            let new_lease_token = Uuid::new_v4().to_string();

            let row: Option<PgRow> = sqlx::query(JOBS_CLAIM_SQL)
                .bind(&wid)
                .bind(lease_duration_ms)
                .bind(&new_lease_token)
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
                let tenant_id: Option<String> = r.try_get(10).ok();
                let assigned_worker: Option<String> = r.try_get(11).ok();
                let available_at_ms: i64 = r.try_get(12).unwrap_or(0);
                let lease_token: Option<String> = r.try_get(13).ok();
                let trace_context: Option<String> =
                    r.try_get(14).ok().or_else(|| r.try_get("trace_context").ok());

                let retry_policy = RetryPolicy {
                    max_attempts: max_attempts.max(1) as u32,
                    initial_backoff_ms: initial_backoff_ms.max(0) as u64,
                    max_backoff_ms: max_backoff_ms.max(0) as u64,
                    backoff_factor,
                };

                let timeout = timeout_ms.map(|ms| Duration::from_millis(ms.max(0) as u64));

                let now = current_epoch_millis();
                let available_at = if available_at_ms <= now {
                    Instant::now()
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
                    tenant_id,
                    assigned_worker,
                    lease_token,
                    available_at,
                    lease_duration: self.default_lease_duration,
                    trace_context,
                }))
            } else {
                Ok(None)
            }
        })
    }

    fn ack(&self, job_id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        let token = lease_token.to_string();
        Box::pin(async move {
            let res = sqlx::query(
                "UPDATE rume_jobs SET status = 'completed', completed_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT, lease_expires_at = NULL WHERE id = $1 AND lease_token = $2 AND status = 'in_flight' AND lease_expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT"
            )
            .bind(&jid)
            .bind(&token)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "cannot ack job {jid}: lease lost, expired, or invalid token"
                )));
            }
            Ok(())
        })
    }

    fn nack(
        &self,
        job: QueuedJob,
        lease_token: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let token = lease_token.to_string();
        Box::pin(async move {
            let delay_ms = delay.as_millis() as i64;

            let res = sqlx::query(
                "UPDATE rume_jobs SET status = 'pending', assigned_worker = NULL, lease_expires_at = NULL, lease_token = NULL, available_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $1 WHERE id = $2 AND lease_token = $3 AND status = 'in_flight' AND lease_expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT"
            )
            .bind(delay_ms)
            .bind(&job.id)
            .bind(&token)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "cannot nack job {}: lease lost, expired, or invalid token",
                    job.id
                )));
            }

            Ok(())
        })
    }

    fn dead_letter(
        &self,
        job: QueuedJob,
        lease_token: &str,
        reason: String,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let token = lease_token.to_string();
        Box::pin(async move {
            let res = sqlx::query(
                "UPDATE rume_jobs SET status = 'dead_letter', assigned_worker = NULL, lease_expires_at = NULL, lease_token = NULL, dead_letter_reason = $1, dead_lettered_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT WHERE id = $2 AND lease_token = $3 AND status = 'in_flight' AND lease_expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT"
            )
            .bind(&reason)
            .bind(&job.id)
            .bind(&token)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "cannot dead_letter job {}: lease lost, expired, or invalid token",
                    job.id
                )));
            }

            Ok(())
        })
    }

    fn renew_lease(
        &self,
        job_id: &str,
        lease_token: &str,
        extension: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        let token = lease_token.to_string();
        Box::pin(async move {
            // Both the new expiry and the validity check use clock_timestamp() so all
            // lease decisions are made by the database clock, not the worker clock.
            let extension_ms = extension.as_millis() as i64;

            let res = sqlx::query(
                "UPDATE rume_jobs \
                 SET lease_expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $1 \
                 WHERE id = $2 AND lease_token = $3 AND status = 'in_flight' \
                   AND lease_expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT"
            )
            .bind(extension_ms)
            .bind(&jid)
            .bind(&token)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "cannot renew lease for job {jid}: lease expired or invalid token"
                )));
            }

            Ok(())
        })
    }

    fn reclaim_abandoned_jobs(
        &self,
        dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>> {
        let workers = dead_workers.to_vec();
        Box::pin(async move {
            let res = sqlx::query(JOBS_REAP_SQL)
                .bind(&workers)
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

    fn list_dead_letters(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<QueuedJob>, JobError>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token FROM rume_jobs WHERE status = 'dead_letter' ORDER BY dead_lettered_at DESC LIMIT $1"
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            let mut jobs = Vec::new();
            for r in rows {
                let id: String = r.try_get(0).map_err(|e| JobError::Fatal(e.to_string()))?;
                let name: String = r.try_get(1).map_err(|e| JobError::Fatal(e.to_string()))?;
                let payload: Vec<u8> = r.try_get(2).map_err(|e| JobError::Fatal(e.to_string()))?;
                let attempt: i32 = r.try_get(3).unwrap_or(1);
                let max_attempts: i32 = r.try_get(4).unwrap_or(3);
                let initial_backoff_ms: i64 = r.try_get(5).unwrap_or(1000);
                let max_backoff_ms: i64 = r.try_get(6).unwrap_or(60000);
                let backoff_factor: f64 = r.try_get(7).unwrap_or(2.0);
                let timeout_ms: Option<i64> = r.try_get(8).ok();
                let idempotency_key: Option<String> = r.try_get(9).ok();
                let tenant_id: Option<String> = r.try_get(10).ok();
                let assigned_worker: Option<String> = r.try_get(11).ok();
                let trace_context: Option<String> = r.try_get("trace_context").ok();
                let retry_policy = RetryPolicy {
                    max_attempts: max_attempts.max(1) as u32,
                    initial_backoff_ms: initial_backoff_ms.max(0) as u64,
                    max_backoff_ms: max_backoff_ms.max(0) as u64,
                    backoff_factor,
                };
                let timeout = timeout_ms.map(|ms| Duration::from_millis(ms.max(0) as u64));
                jobs.push(QueuedJob {
                    id,
                    name,
                    payload,
                    attempt: attempt.max(1) as u32,
                    retry_policy,
                    timeout,
                    idempotency_key,
                    tenant_id,
                    available_at: Instant::now(),
                    lease_token: None,
                    assigned_worker,
                    lease_duration: self.default_lease_duration,
                    trace_context,
                });
            }
            Ok(jobs)
        })
    }

    fn replay_dead_letter(
        &self,
        job_id: &str,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        Box::pin(async move {
            let res = sqlx::query(
                "UPDATE rume_jobs SET status = 'pending', attempt = 0, assigned_worker = NULL, lease_expires_at = NULL, lease_token = NULL, dead_letter_reason = NULL, dead_lettered_at = NULL, available_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT WHERE id = $1 AND status = 'dead_letter'"
            )
            .bind(&jid)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!("dead letter job {jid} not found or not in dead_letter status")));
            }
            Ok(())
        })
    }
}
