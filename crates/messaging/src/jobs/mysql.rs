//! MySQL-backed durable background job queue using SQLx.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use sqlx::mysql::MySqlRow;
use sqlx::{MySqlPool, Row};
use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob, RawJob};
use crate::jobs::queue::JobQueue;
use crate::jobs::retry::RetryPolicy;

fn current_epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn row_to_queued_job(
    r: &MySqlRow,
    lease_duration: Duration,
    attempt_override: Option<u32>,
    assigned_worker_override: Option<String>,
    lease_token_override: Option<String>,
) -> Result<QueuedJob, JobError> {
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
    let assigned_worker: Option<String> = assigned_worker_override.or_else(|| r.try_get(11).ok());
    let available_at_ms: i64 = r.try_get(12).unwrap_or(0);
    let lease_token: Option<String> = lease_token_override.or_else(|| r.try_get(13).ok());
    let trace_context: Option<String> = r.try_get(14).ok();
    let queue: String = r.try_get(15).unwrap_or_else(|_| "default".to_string());
    let priority: i32 = r.try_get(16).unwrap_or(0);

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

    Ok(QueuedJob {
        id,
        name,
        queue,
        priority,
        payload,
        attempt: attempt_override.unwrap_or_else(|| attempt.max(1) as u32),
        retry_policy,
        timeout,
        idempotency_key,
        tenant_id,
        assigned_worker,
        lease_token,
        available_at,
        lease_duration,
        trace_context,
    })
}

/// MySQL-backed implementation of [`JobQueue`].
#[derive(Clone)]
pub struct MysqlJobQueue {
    pool: MySqlPool,
    default_lease_duration: Duration,
}

impl Debug for MysqlJobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlJobQueue")
            .field("default_lease_duration", &self.default_lease_duration)
            .finish()
    }
}

impl MysqlJobQueue {
    /// Schema DDL for durable background jobs in MySQL.
    pub const SCHEMA: &'static str = rume_data::sql::MYSQL_JOBS_SCHEMA;

    /// Creates a new MySQL job queue with a 60-second default lease duration.
    pub fn new(pool: MySqlPool) -> Self {
        Self {
            pool,
            default_lease_duration: Duration::from_secs(60),
        }
    }

    /// Sets the worker claim lease duration.
    pub fn with_lease_duration(mut self, duration: Duration) -> Self {
        self.default_lease_duration = duration;
        self
    }

    /// Returns a reference to the underlying MySQL connection pool.
    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// Initializes the `rume_jobs` table and indexes if not already present.
    pub async fn init_schema(&self) -> Result<(), JobError> {
        sqlx::raw_sql(Self::SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(format!("failed to init mysql jobs schema: {e}")))?;
        Ok(())
    }
}

impl JobQueue for MysqlJobQueue {
    fn enqueue_delayed_with_trace<J: Job>(
        &self,
        job: J,
        delay: Duration,
        trace_context: Option<String>,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        match RawJob::from_job(job, trace_context) {
            Ok(raw) => self.enqueue_raw(raw, delay),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn enqueue_raw(&self, job: RawJob, delay: Duration) -> BoxFuture<'_, Result<String, JobError>> {
        let id = Uuid::new_v4().to_string();
        Box::pin(async move {
            let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
            let max_attempts = i32::try_from(job.retry_policy.max_attempts).unwrap_or(i32::MAX);
            let initial_backoff_ms =
                i64::try_from(job.retry_policy.initial_backoff_ms).unwrap_or(i64::MAX);
            let max_backoff_ms = i64::try_from(job.retry_policy.max_backoff_ms).unwrap_or(i64::MAX);
            let backoff_factor = job.retry_policy.backoff_factor;
            let timeout_ms = job
                .timeout
                .map(|t| i64::try_from(t.as_millis()).unwrap_or(i64::MAX));

            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_INSERT)
                .bind(&id)
                .bind(&job.name)
                .bind(&job.queue)
                .bind(job.priority)
                .bind(&job.payload)
                .bind(max_attempts)
                .bind(initial_backoff_ms)
                .bind(max_backoff_ms)
                .bind(backoff_factor)
                .bind(timeout_ms)
                .bind(&job.idempotency_key)
                .bind(&job.tenant_id)
                .bind(delay_ms)
                .bind(&job.trace_context)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() > 0 {
                Ok(id)
            } else if let Some(ref ikey) = job.idempotency_key {
                let existing = sqlx::query(rume_data::sql::MYSQL_JOBS_IDEMPOTENCY_CHECK)
                    .bind(&job.tenant_id)
                    .bind(ikey)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;

                if let Some(erow) = existing {
                    let existing_id: String = erow
                        .try_get(0)
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    Ok(existing_id)
                } else {
                    Err(JobError::Fatal(
                        "idempotency conflict race: conflicting row is no longer active".into(),
                    ))
                }
            } else {
                Err(JobError::Fatal(
                    "conflict detected but no idempotency key was supplied".into(),
                ))
            }
        })
    }

    fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        self.dequeue_from(worker_id, None)
    }

    fn dequeue_from(
        &self,
        worker_id: &str,
        queues: Option<&[&str]>,
    ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        let wid = worker_id.to_string();
        let queue_filter: Option<Vec<String>> =
            queues.map(|qs| qs.iter().map(|s| (*s).to_string()).collect());
        Box::pin(async move {
            if queue_filter.as_ref().is_some_and(Vec::is_empty) {
                return Ok(None);
            }
            let lease_duration_ms =
                i64::try_from(self.default_lease_duration.as_millis()).unwrap_or(i64::MAX);
            let new_lease_token = Uuid::new_v4().to_string();

            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let row: Option<MySqlRow> = if let Some(qs) = queue_filter {
                let placeholders = vec!["?"; qs.len()].join(", ");
                let sql = format!(
                    "SELECT id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token, trace_context, queue, priority
FROM rume_jobs
WHERE queue IN ({placeholders})
  AND ((status = 'pending' AND available_at <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000))
   OR (status = 'in_flight' AND lease_expires_at <= FLOOR(UNIX_TIMESTAMP(NOW(3)) * 1000)))
ORDER BY priority DESC, available_at ASC, created_at ASC
LIMIT 1
FOR UPDATE SKIP LOCKED;"
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
                for name in &qs {
                    q = q.bind(name);
                }
                q.fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?
            } else {
                sqlx::query(rume_data::sql::MYSQL_JOBS_CLAIM)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?
            };

            let Some(r) = row else {
                tx.commit()
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;
                return Ok(None);
            };

            let id: String = r.try_get(0).map_err(|e| JobError::Fatal(e.to_string()))?;
            let prev_attempt: i32 = r.try_get(3).unwrap_or(0);
            let next_attempt = (prev_attempt + 1).max(1) as u32;

            sqlx::query(rume_data::sql::MYSQL_JOBS_CLAIM_UPDATE)
                .bind(&wid)
                .bind(lease_duration_ms)
                .bind(&new_lease_token)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let job = row_to_queued_job(
                &r,
                self.default_lease_duration,
                Some(next_attempt),
                Some(wid),
                Some(new_lease_token),
            )?;
            Ok(Some(job))
        })
    }

    fn reclaim_abandoned_jobs(
        &self,
        dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>> {
        let workers = dead_workers.to_vec();
        Box::pin(async move {
            if workers.is_empty() {
                return Ok(0);
            }
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let mut reclaimed: usize = 0;
            for wid in &workers {
                let res = sqlx::query(rume_data::sql::MYSQL_JOBS_REAP)
                    .bind(wid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;
                reclaimed += res.rows_affected() as usize;
            }
            tx.commit()
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            Ok(reclaimed)
        })
    }

    fn ack(&self, job_id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        let token = lease_token.to_string();
        Box::pin(async move {
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_ACK)
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
            let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_NACK)
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

    fn defer(
        &self,
        job: QueuedJob,
        lease_token: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let token = lease_token.to_string();
        Box::pin(async move {
            let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_DEFER)
                .bind(delay_ms)
                .bind(&job.id)
                .bind(&token)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "cannot defer job {}: lease lost, expired, or invalid token",
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
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_DEAD_LETTER)
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
            let extension_ms = i64::try_from(extension.as_millis()).unwrap_or(i64::MAX);
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_RENEW_LEASE)
                .bind(extension_ms)
                .bind(&jid)
                .bind(&token)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "cannot renew lease for job {jid}: invalid lease token or job no longer in flight"
                )));
            }
            Ok(())
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let row = sqlx::query(rume_data::sql::MYSQL_JOBS_PENDING_COUNT)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let row = sqlx::query(rume_data::sql::MYSQL_JOBS_DEAD_LETTER_COUNT)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn list_dead_letters(&self, limit: usize) -> BoxFuture<'_, Result<Vec<QueuedJob>, JobError>> {
        Box::pin(async move {
            let rows = sqlx::query(rume_data::sql::MYSQL_JOBS_LIST_DEAD_LETTERS)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            rows.iter()
                .map(|r| row_to_queued_job(r, self.default_lease_duration, None, None, None))
                .collect()
        })
    }

    fn purge_completed(&self, retention: Duration) -> BoxFuture<'_, Result<u64, JobError>> {
        let retention_ms = i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
        Box::pin(async move {
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_PURGE)
                .bind(retention_ms)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            Ok(res.rows_affected())
        })
    }

    fn replay_dead_letter(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        Box::pin(async move {
            let res = sqlx::query(rume_data::sql::MYSQL_JOBS_REPLAY_DEAD_LETTER)
                .bind(&jid)
                .execute(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(JobError::Fatal(format!(
                    "dead letter job {jid} not found or not in dead_letter status"
                )));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mysql_job_queue_sql_constants_are_populated() {
        assert!(!rume_data::sql::MYSQL_JOBS_SCHEMA.trim().is_empty());
        assert!(rume_data::sql::MYSQL_JOBS_CLAIM.contains("FOR UPDATE SKIP LOCKED"));
        assert!(rume_data::sql::MYSQL_JOBS_CLAIM.contains("ORDER BY priority DESC"));
        assert!(rume_data::sql::MYSQL_JOBS_DEFER.contains("GREATEST(attempt - 1, 0)"));
    }
}
