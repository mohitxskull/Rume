//! SQLite-backed durable background job queue using SQLx.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
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

fn row_to_queued_job(r: &SqliteRow, lease_duration: Duration) -> Result<QueuedJob, JobError> {
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
        attempt: attempt.max(1) as u32,
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

/// SQLite-backed implementation of [`JobQueue`].
#[derive(Clone)]
pub struct SqliteJobQueue {
    pool: SqlitePool,
    default_lease_duration: Duration,
}

impl Debug for SqliteJobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteJobQueue")
            .field("default_lease_duration", &self.default_lease_duration)
            .finish()
    }
}

impl SqliteJobQueue {
    /// Schema DDL for durable background jobs in SQLite.
    pub const SCHEMA: &'static str = rume_data::sql::SQLITE_JOBS_SCHEMA;

    /// Creates a new SQLite job queue with a 60-second default lease duration.
    pub fn new(pool: SqlitePool) -> Self {
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

    /// Returns a reference to the underlying SQLite connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Initializes the `rume_jobs` table and indexes if not already present.
    pub async fn init_schema(&self) -> Result<(), JobError> {
        sqlx::raw_sql(Self::SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::Fatal(format!("failed to init sqlite jobs schema: {e}")))?;
        Ok(())
    }
}

impl JobQueue for SqliteJobQueue {
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

            let row = sqlx::query(rume_data::sql::SQLITE_JOBS_INSERT)
                .bind(&id)
                .bind(&job.name)
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
                .bind(&job.queue)
                .bind(job.priority)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if let Some(r) = row {
                let inserted_id: String = r.try_get(0).unwrap_or(id);
                Ok(inserted_id)
            } else if let Some(ref ikey) = job.idempotency_key {
                let existing = sqlx::query(rume_data::sql::SQLITE_JOBS_IDEMPOTENCY_CHECK)
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

            let row: Option<SqliteRow> = if let Some(qs) = queue_filter {
                // Why: SQLite lacks Postgres's `= ANY($1)` array binding, so `IN (?4, ...)`
                // is built from a strict `?` placeholder count and bound via `.bind()`
                // parameters with `AssertSqlSafe` (no user input is interpolated into SQL).
                let placeholders = (0..qs.len())
                    .map(|idx| format!("?{}", idx + 4))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "UPDATE rume_jobs
SET status = 'in_flight',
    assigned_worker = ?1,
    attempt = attempt + 1,
    lease_expires_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?2,
    lease_token = ?3
WHERE id = (
    SELECT id FROM rume_jobs
    WHERE queue IN ({placeholders})
      AND ((status = 'pending' AND available_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
       OR (status = 'in_flight' AND lease_expires_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)))
    ORDER BY priority DESC, available_at ASC, created_at ASC
    LIMIT 1
)
RETURNING id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token, trace_context, queue, priority;"
                );
                let mut q = sqlx::query(sqlx::AssertSqlSafe(sql))
                    .bind(&wid)
                    .bind(lease_duration_ms)
                    .bind(&new_lease_token);
                for name in &qs {
                    q = q.bind(name);
                }
                q.fetch_optional(&self.pool)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?
            } else {
                sqlx::query(rume_data::sql::SQLITE_JOBS_CLAIM)
                    .bind(&wid)
                    .bind(lease_duration_ms)
                    .bind(&new_lease_token)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?
            };

            row.as_ref()
                .map(|r| row_to_queued_job(r, self.default_lease_duration))
                .transpose()
        })
    }

    fn dequeue_excluding(
        &self,
        worker_id: &str,
        excluded_queues: &[String],
    ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        if excluded_queues.is_empty() {
            return self.dequeue_for(worker_id);
        }
        let wid = worker_id.to_string();
        let excluded: Vec<String> = excluded_queues.to_vec();
        Box::pin(async move {
            let lease_duration_ms =
                i64::try_from(self.default_lease_duration.as_millis()).unwrap_or(i64::MAX);
            let new_lease_token = Uuid::new_v4().to_string();
            // Why: SQLite lacks array bindings; build positional `?4, ?5, ...` placeholders
            // and bind each excluded queue name via `.bind()` (`RC-30`).
            let placeholders = (0..excluded.len())
                .map(|idx| format!("?{}", idx + 4))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "UPDATE rume_jobs
SET status = 'in_flight',
    assigned_worker = ?1,
    attempt = attempt + 1,
    lease_expires_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) + ?2,
    lease_token = ?3
WHERE id = (
    SELECT id FROM rume_jobs
    WHERE queue NOT IN ({placeholders})
      AND ((status = 'pending' AND available_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
       OR (status = 'in_flight' AND lease_expires_at <= CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)))
    ORDER BY priority DESC, available_at ASC, created_at ASC
    LIMIT 1
)
RETURNING id, name, payload, attempt, max_attempts, initial_backoff_ms, max_backoff_ms, backoff_factor, timeout_ms, idempotency_key, tenant_id, assigned_worker, available_at, lease_token, trace_context, queue, priority;"
            );
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(&wid)
                .bind(lease_duration_ms)
                .bind(&new_lease_token);
            for name in &excluded {
                q = q.bind(name);
            }
            let row: Option<SqliteRow> = q
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            row.as_ref()
                .map(|r| row_to_queued_job(r, self.default_lease_duration))
                .transpose()
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
                let res = sqlx::query(rume_data::sql::SQLITE_JOBS_REAP)
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
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_ACK)
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
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_NACK)
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
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_DEFER)
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
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_DEAD_LETTER)
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
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_RENEW_LEASE)
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
            let row = sqlx::query(rume_data::sql::SQLITE_JOBS_PENDING_COUNT)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let row = sqlx::query(rume_data::sql::SQLITE_JOBS_DEAD_LETTER_COUNT)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn list_dead_letters(&self, limit: usize) -> BoxFuture<'_, Result<Vec<QueuedJob>, JobError>> {
        Box::pin(async move {
            let rows = sqlx::query(rume_data::sql::SQLITE_JOBS_LIST_DEAD_LETTERS)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            rows.iter()
                .map(|r| row_to_queued_job(r, self.default_lease_duration))
                .collect()
        })
    }

    fn purge_completed(&self, retention: Duration) -> BoxFuture<'_, Result<u64, JobError>> {
        let retention_ms = i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
        Box::pin(async move {
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_PURGE)
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
            let res = sqlx::query(rume_data::sql::SQLITE_JOBS_REPLAY_DEAD_LETTER)
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
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct SampleJob {
        msg: String,
    }

    impl Job for SampleJob {
        const NAME: &'static str = "sample_job";
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct PriorityJob {
        label: String,
        prio: i32,
        target_queue: String,
    }

    impl Job for PriorityJob {
        const NAME: &'static str = "priority_job";

        fn queue(&self) -> &str {
            &self.target_queue
        }

        fn priority(&self) -> i32 {
            self.prio
        }
    }

    async fn setup_sqlite_queue() -> SqliteJobQueue {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        let queue = SqliteJobQueue::new(pool);
        queue.init_schema().await.expect("init_schema");
        queue
    }

    #[tokio::test]
    async fn sqlite_job_queue_enqueues_dequeues_acks_and_dead_letters() {
        let queue = setup_sqlite_queue().await;

        let id1 = queue
            .enqueue(SampleJob {
                msg: "first".into(),
            })
            .await
            .expect("enqueue first");
        let id2 = queue
            .enqueue(SampleJob {
                msg: "second".into(),
            })
            .await
            .expect("enqueue second");
        assert_eq!(queue.pending_count().await.expect("pending_count"), 2);

        let claimed1 = queue
            .dequeue_for("worker-a")
            .await
            .expect("dequeue 1")
            .expect("job 1 present");
        assert_eq!(claimed1.id, id1);
        assert_eq!(claimed1.attempt, 1);
        let token1 = claimed1.lease_token.clone().expect("lease_token 1");

        queue.ack(&claimed1.id, &token1).await.expect("ack job 1");
        assert_eq!(queue.pending_count().await.expect("pending_count"), 1);

        let claimed2 = queue
            .dequeue_for("worker-a")
            .await
            .expect("dequeue 2")
            .expect("job 2 present");
        assert_eq!(claimed2.id, id2);
        let token2 = claimed2.lease_token.clone().expect("lease_token 2");

        queue
            .dead_letter(claimed2, &token2, "fatal error".into())
            .await
            .expect("dead_letter job 2");
        assert_eq!(queue.pending_count().await.expect("pending_count"), 0);
        assert_eq!(
            queue.dead_letter_count().await.expect("dead_letter_count"),
            1
        );

        let dlq = queue
            .list_dead_letters(10)
            .await
            .expect("list_dead_letters");
        assert_eq!(dlq.len(), 1);
        assert_eq!(dlq[0].id, id2);

        queue
            .replay_dead_letter(&id2)
            .await
            .expect("replay_dead_letter");
        assert_eq!(
            queue.dead_letter_count().await.expect("dlq after replay"),
            0
        );
        assert_eq!(
            queue.pending_count().await.expect("pending after replay"),
            1
        );
    }

    #[tokio::test]
    async fn job_queue_orders_by_priority_desc_then_available_at() {
        let queue = setup_sqlite_queue().await;

        let low_id = queue
            .enqueue(PriorityJob {
                label: "low".into(),
                prio: 1,
                target_queue: "default".into(),
            })
            .await
            .expect("enqueue low");
        let high_id = queue
            .enqueue(PriorityJob {
                label: "high".into(),
                prio: 100,
                target_queue: "default".into(),
            })
            .await
            .expect("enqueue high");
        let med_id = queue
            .enqueue(PriorityJob {
                label: "med".into(),
                prio: 50,
                target_queue: "default".into(),
            })
            .await
            .expect("enqueue med");

        let j1 = queue.dequeue_for("w1").await.unwrap().unwrap();
        let j2 = queue.dequeue_for("w1").await.unwrap().unwrap();
        let j3 = queue.dequeue_for("w1").await.unwrap().unwrap();

        assert_eq!(j1.id, high_id);
        assert_eq!(j1.priority, 100);
        assert_eq!(j2.id, med_id);
        assert_eq!(j2.priority, 50);
        assert_eq!(j3.id, low_id);
        assert_eq!(j3.priority, 1);
    }

    #[tokio::test]
    async fn job_queue_filters_by_named_queue() {
        let queue = setup_sqlite_queue().await;

        let email_id = queue
            .enqueue(PriorityJob {
                label: "email".into(),
                prio: 0,
                target_queue: "emails".into(),
            })
            .await
            .expect("enqueue email");
        let billing_id = queue
            .enqueue(PriorityJob {
                label: "billing".into(),
                prio: 10,
                target_queue: "billing".into(),
            })
            .await
            .expect("enqueue billing");

        let claimed_email = queue
            .dequeue_from("w1", Some(&["emails"]))
            .await
            .expect("dequeue emails")
            .expect("email job present");
        assert_eq!(claimed_email.id, email_id);
        assert_eq!(claimed_email.queue, "emails");

        // No more jobs in "emails", even though "billing" has a higher-priority job.
        assert!(
            queue
                .dequeue_from("w1", Some(&["emails"]))
                .await
                .expect("dequeue empty emails")
                .is_none()
        );

        let claimed_billing = queue
            .dequeue_from("w1", Some(&["billing"]))
            .await
            .expect("dequeue billing")
            .expect("billing job present");
        assert_eq!(claimed_billing.id, billing_id);
        assert_eq!(claimed_billing.queue, "billing");
    }

    #[tokio::test]
    async fn sqlite_job_queue_defer_idempotency_and_reclaim() {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct IdempJob {
            key: String,
        }

        impl Job for IdempJob {
            const NAME: &'static str = "idemp_job";

            fn idempotency_key(&self) -> Option<String> {
                Some(self.key.clone())
            }
        }

        let queue = setup_sqlite_queue().await;

        let id1 = queue
            .enqueue(IdempJob {
                key: "dedup-1".into(),
            })
            .await
            .expect("enqueue 1");
        let id2 = queue
            .enqueue(IdempJob {
                key: "dedup-1".into(),
            })
            .await
            .expect("enqueue duplicate");
        assert_eq!(id1, id2);
        assert_eq!(queue.pending_count().await.unwrap(), 1);

        let claimed = queue.dequeue_for("worker-1").await.unwrap().unwrap();
        assert_eq!(claimed.attempt, 1);
        let tok = claimed.lease_token.clone().unwrap();

        queue
            .renew_lease(&claimed.id, &tok, Duration::from_secs(30))
            .await
            .expect("renew_lease");

        // Defer must decrement attempt back to 0 so next dequeue sees attempt == 1.
        queue
            .defer(claimed, &tok, Duration::ZERO)
            .await
            .expect("defer");
        let reclaimed = queue.dequeue_for("worker-2").await.unwrap().unwrap();
        assert_eq!(reclaimed.attempt, 1);

        let reaped = queue
            .reclaim_abandoned_jobs(&["worker-2".to_string()])
            .await
            .expect("reclaim_abandoned_jobs");
        assert_eq!(reaped, 1);
        assert_eq!(queue.pending_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn sqlite_renew_lease_rejects_already_expired_lease() {
        #[derive(Debug, Serialize, Deserialize)]
        struct ShortLeaseJob;

        impl Job for ShortLeaseJob {
            const NAME: &'static str = "test.short_lease";
        }

        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let queue = SqliteJobQueue::new(pool).with_lease_duration(Duration::from_millis(25));
        queue.init_schema().await.unwrap();

        queue.enqueue(ShortLeaseJob).await.unwrap();
        let claimed = queue.dequeue_for("worker-1").await.unwrap().unwrap();
        let tok = claimed.lease_token.clone().unwrap();

        // Wait past the 25ms lease expiry without any peer reclaiming the job yet.
        tokio::time::sleep(Duration::from_millis(60)).await;

        let err = queue
            .renew_lease(&claimed.id, &tok, Duration::from_secs(30))
            .await
            .expect_err("renewing an already-expired lease must fail");
        assert!(
            matches!(err, JobError::Fatal(_)),
            "expected Fatal error on expired lease renewal, got {err:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_poison_pill_job_dead_letters_without_reexecuting_handler() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::jobs::{JobContext, JobHandler, JobRegistry, JobWorker};

        #[derive(Debug, Serialize, Deserialize)]
        struct PoisonJob {
            id: String,
        }

        impl Job for PoisonJob {
            const NAME: &'static str = "test.poison_pill";

            fn retry_policy(&self) -> RetryPolicy {
                RetryPolicy::new().with_max_attempts(1)
            }
        }

        struct CountingHandler(Arc<AtomicUsize>);

        impl JobHandler<PoisonJob> for CountingHandler {
            fn handle<'a>(
                &'a self,
                _ctx: &'a JobContext,
                _job: PoisonJob,
            ) -> BoxFuture<'a, Result<(), JobError>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        }

        let backend = rume_data::persistence::SqliteBackend::in_memory()
            .await
            .expect("sqlite");
        let queue = Arc::new(
            SqliteJobQueue::new(backend.pool().clone())
                .with_lease_duration(Duration::from_millis(50)),
        );
        queue.init_schema().await.expect("init_schema");

        queue
            .enqueue(PoisonJob { id: "p-1".into() })
            .await
            .expect("enqueue");

        // Simulate worker 1 claiming the job (attempt = 1 == max_attempts) and crashing without ack.
        let claimed = queue
            .dequeue_for("crashed-worker")
            .await
            .expect("dequeue")
            .expect("job claimed");
        assert_eq!(claimed.attempt, 1);

        // Wait for lease to expire.
        tokio::time::sleep(Duration::from_millis(90)).await;

        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = JobRegistry::new();
        registry.register(CountingHandler(Arc::clone(&executions)));

        let worker = JobWorker::with_concurrency_limit(Arc::clone(&queue), Arc::new(registry), 2);
        let handle = worker.start();

        // Wait until the worker claims the expired row and dead-letters it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if queue.dead_letter_count().await.expect("dlq count") == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }

        assert!(
            handle.shutdown(Duration::from_secs(2)).await,
            "worker shutdown must drain cleanly"
        );

        assert_eq!(
            queue.dead_letter_count().await.expect("dlq count"),
            1,
            "expired max_attempts=1 job must be dead-lettered"
        );
        assert_eq!(queue.pending_count().await.expect("pending count"), 0);
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "handler must NOT be re-executed after attempt exceeded max_attempts"
        );
    }

    #[tokio::test]
    async fn sqlite_worker_fast_queue_not_blocked_by_saturated_slow_queue() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Notify;

        use crate::jobs::{JobContext, JobHandler, JobRegistry, JobWorker};

        #[derive(Debug, Serialize, Deserialize)]
        struct SlowJob;

        impl Job for SlowJob {
            const NAME: &'static str = "test.slow_queue_job";
            fn queue(&self) -> &str {
                "slow"
            }
        }

        #[derive(Debug, Serialize, Deserialize)]
        struct FastJob;

        impl Job for FastJob {
            const NAME: &'static str = "test.fast_queue_job";
            fn queue(&self) -> &str {
                "fast"
            }
        }

        struct SlowHandler {
            started: Arc<AtomicUsize>,
            completed: Arc<AtomicUsize>,
            release: Arc<Notify>,
        }

        impl JobHandler<SlowJob> for SlowHandler {
            fn handle<'a>(
                &'a self,
                _ctx: &'a JobContext,
                _job: SlowJob,
            ) -> BoxFuture<'a, Result<(), JobError>> {
                let started = Arc::clone(&self.started);
                let completed = Arc::clone(&self.completed);
                let release = Arc::clone(&self.release);
                Box::pin(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    release.notified().await;
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        }

        struct FastHandler(Arc<AtomicUsize>);

        impl JobHandler<FastJob> for FastHandler {
            fn handle<'a>(
                &'a self,
                _ctx: &'a JobContext,
                _job: FastJob,
            ) -> BoxFuture<'a, Result<(), JobError>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        }

        let backend = rume_data::persistence::SqliteBackend::in_memory()
            .await
            .expect("sqlite");
        let queue = Arc::new(SqliteJobQueue::new(backend.pool().clone()));
        queue.init_schema().await.expect("init_schema");

        // Enqueue 2 slow jobs (slow queue concurrency = 1) and 2 fast jobs.
        queue.enqueue(SlowJob).await.expect("slow 1");
        queue.enqueue(SlowJob).await.expect("slow 2");
        queue.enqueue(FastJob).await.expect("fast 1");
        queue.enqueue(FastJob).await.expect("fast 2");

        let slow_started = Arc::new(AtomicUsize::new(0));
        let slow_completed = Arc::new(AtomicUsize::new(0));
        let slow_release = Arc::new(Notify::new());
        let fast_completed = Arc::new(AtomicUsize::new(0));

        let mut registry = JobRegistry::new();
        registry.register(SlowHandler {
            started: Arc::clone(&slow_started),
            completed: Arc::clone(&slow_completed),
            release: Arc::clone(&slow_release),
        });
        registry.register(FastHandler(Arc::clone(&fast_completed)));

        let worker = JobWorker::with_concurrency_limit(Arc::clone(&queue), Arc::new(registry), 4)
            .with_queues(["slow", "fast"])
            .with_queue_concurrency("slow", 1);
        let handle = worker.start();

        // Both fast jobs must complete while the slow queue is saturated with slow_1 still running.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if fast_completed.load(Ordering::SeqCst) == 2
                && slow_started.load(Ordering::SeqCst) == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }

        assert_eq!(
            fast_completed.load(Ordering::SeqCst),
            2,
            "fast queue jobs must complete while slow queue is saturated"
        );
        assert_eq!(
            slow_started.load(Ordering::SeqCst),
            1,
            "slow queue concurrency=1 must allow only 1 slow job in flight"
        );
        assert_eq!(slow_completed.load(Ordering::SeqCst), 0);

        // Release both slow jobs one by one.
        slow_release.notify_one();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if slow_started.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        slow_release.notify_one();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if slow_completed.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        assert_eq!(slow_completed.load(Ordering::SeqCst), 2);

        assert!(
            handle.shutdown(Duration::from_secs(2)).await,
            "worker shutdown must drain cleanly"
        );
    }

    #[tokio::test]
    async fn sqlite_worker_unrestricted_queues_excludes_saturated_high_priority_queue() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Notify;

        use crate::jobs::job::{JobContext, JobHandler};
        use crate::jobs::worker::{JobRegistry, JobWorker};

        #[derive(Debug, Serialize, Deserialize)]
        struct HighPrioSlowJob {
            id: u32,
        }
        impl Job for HighPrioSlowJob {
            const NAME: &'static str = "rc30_high_prio_slow";
            fn queue(&self) -> &str {
                "slow_hp"
            }
            fn priority(&self) -> i32 {
                100
            }
        }

        #[derive(Debug, Serialize, Deserialize)]
        struct LowPrioFastJob {
            id: u32,
        }
        impl Job for LowPrioFastJob {
            const NAME: &'static str = "rc30_low_prio_fast";
            fn queue(&self) -> &str {
                "fast_lp"
            }
            fn priority(&self) -> i32 {
                0
            }
        }

        struct BlockingSlowHandler {
            started: Arc<AtomicUsize>,
            completed: Arc<AtomicUsize>,
            release: Arc<Notify>,
        }
        impl JobHandler<HighPrioSlowJob> for BlockingSlowHandler {
            fn handle<'a>(
                &'a self,
                _ctx: &'a JobContext,
                _job: HighPrioSlowJob,
            ) -> BoxFuture<'a, Result<(), JobError>> {
                let started = Arc::clone(&self.started);
                let completed = Arc::clone(&self.completed);
                let release = Arc::clone(&self.release);
                Box::pin(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    release.notified().await;
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        }

        struct FastHandler {
            completed: Arc<AtomicUsize>,
        }
        impl JobHandler<LowPrioFastJob> for FastHandler {
            fn handle<'a>(
                &'a self,
                _ctx: &'a JobContext,
                _job: LowPrioFastJob,
            ) -> BoxFuture<'a, Result<(), JobError>> {
                let completed = Arc::clone(&self.completed);
                Box::pin(async move {
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }
        }

        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("sqlite memory");
        let queue = Arc::new(SqliteJobQueue::new(pool));
        queue.init_schema().await.expect("init schema");

        // Enqueue 2 high-priority slow jobs and 1 low-priority fast job.
        queue.enqueue(HighPrioSlowJob { id: 1 }).await.unwrap();
        queue.enqueue(HighPrioSlowJob { id: 2 }).await.unwrap();
        queue.enqueue(LowPrioFastJob { id: 1 }).await.unwrap();

        let slow_started = Arc::new(AtomicUsize::new(0));
        let slow_completed = Arc::new(AtomicUsize::new(0));
        let slow_release = Arc::new(Notify::new());
        let fast_completed = Arc::new(AtomicUsize::new(0));

        let mut registry = JobRegistry::new();
        registry.register(BlockingSlowHandler {
            started: Arc::clone(&slow_started),
            completed: Arc::clone(&slow_completed),
            release: Arc::clone(&slow_release),
        });
        registry.register(FastHandler {
            completed: Arc::clone(&fast_completed),
        });

        // Notice: `queues == None` (no `.with_queues(...)`), only `.with_queue_concurrency("slow_hp", 1)`.
        let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry))
            .with_concurrency(4)
            .with_queue_concurrency("slow_hp", 1);

        let handle = worker.start();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if fast_completed.load(Ordering::SeqCst) == 1
                && slow_started.load(Ordering::SeqCst) == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }

        assert_eq!(
            fast_completed.load(Ordering::SeqCst),
            1,
            "low-priority fast queue must complete immediately without starvation when high-priority slow queue is saturated (queues == None)"
        );
        assert_eq!(slow_started.load(Ordering::SeqCst), 1);
        assert_eq!(slow_completed.load(Ordering::SeqCst), 0);

        // Release both slow jobs and shut down cleanly.
        slow_release.notify_one();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if slow_started.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        slow_release.notify_one();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if slow_completed.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        assert_eq!(slow_completed.load(Ordering::SeqCst), 2);
        assert!(handle.shutdown(Duration::from_secs(2)).await);
    }
}
