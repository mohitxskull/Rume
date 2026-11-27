//! Job batch orchestration (`JobBatch`, `BatchStore`, `InMemoryBatchStore`, `SqlBatchStore`).

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob, RawJob};
use crate::jobs::queue::JobQueue;
use crate::jobs::retry::RetryPolicy;
use crate::jobs::worker::JobRegistry;

/// Unique identifier for a [`JobBatch`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BatchId(pub String);

/// Lifecycle status of a [`JobBatch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    /// Batch is actively executing jobs.
    Running,
    /// All jobs in the batch completed without failure.
    Completed,
    /// At least one job in the batch failed/dead-lettered.
    Failed,
    /// Batch was explicitly cancelled.
    Cancelled,
}

/// Persisted state of a job batch and its optional completion callbacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobBatch {
    /// Unique batch identifier.
    pub id: BatchId,
    /// Human-readable batch name.
    pub name: String,
    /// Total number of jobs in the batch.
    pub total_jobs: u32,
    /// Remaining pending jobs.
    pub pending_jobs: u32,
    /// Number of successfully completed jobs.
    pub completed_jobs: u32,
    /// Number of failed jobs.
    pub failed_jobs: u32,
    /// Current lifecycle status.
    pub status: BatchStatus,
    /// Optional callback job name invoked only when all jobs succeed.
    pub then_job_name: Option<String>,
    /// Serialized payload for `then` callback job.
    pub then_job_payload: Option<Vec<u8>>,
    /// Optional callback job name invoked when the batch reaches any terminal state.
    pub finally_job_name: Option<String>,
    /// Serialized payload for `finally` callback job.
    pub finally_job_payload: Option<Vec<u8>>,
    /// Whether terminal callbacks have already been dispatched.
    pub callbacks_dispatched: bool,
}

impl JobBatch {
    fn callback_specs(&self) -> Vec<(String, Vec<u8>, String)> {
        let mut specs = Vec::new();
        if self.status == BatchStatus::Completed
            && let (Some(name), Some(payload)) = (&self.then_job_name, &self.then_job_payload)
        {
            specs.push((
                name.clone(),
                payload.clone(),
                format!("batch:{}:then", self.id.0),
            ));
        }
        if matches!(
            self.status,
            BatchStatus::Completed | BatchStatus::Failed | BatchStatus::Cancelled
        ) && let (Some(name), Some(payload)) =
            (&self.finally_job_name, &self.finally_job_payload)
        {
            specs.push((
                name.clone(),
                payload.clone(),
                format!("batch:{}:finally", self.id.0),
            ));
        }
        specs
    }

    /// Dispatches terminal callbacks (`then` if all succeeded, `finally` on any terminal state)
    /// directly via `registry`, claiming the one-shot dispatch lock in `store`.
    pub async fn dispatch_callbacks<S: BatchStore>(
        &self,
        store: &S,
        registry: &JobRegistry,
    ) -> Result<(), JobError> {
        let specs = self.callback_specs();
        if specs.is_empty() || !store.try_claim_callbacks(&self.id).await? {
            return Ok(());
        }
        for (name, payload, idemp_key) in specs {
            let queued = QueuedJob {
                id: Uuid::new_v4().to_string(),
                name,
                queue: "default".to_string(),
                priority: 0,
                payload,
                attempt: 1,
                retry_policy: RetryPolicy::default(),
                timeout: Some(Duration::from_secs(60)),
                idempotency_key: Some(idemp_key),
                tenant_id: None,
                assigned_worker: None,
                lease_token: None,
                available_at: Instant::now(),
                lease_duration: Duration::from_secs(60),
                trace_context: None,
            };
            if let Err(err) = registry.dispatch(&queued).await {
                let _ = store.release_callbacks_claim(&self.id).await;
                return Err(err);
            }
        }
        Ok(())
    }

    /// Enqueues terminal callbacks (`then` if all succeeded, `finally` on any terminal state)
    /// onto `queue`, claiming the one-shot dispatch lock in `store`.
    pub async fn dispatch_callbacks_to_queue<S: BatchStore, Q: JobQueue>(
        &self,
        store: &S,
        queue: &Q,
    ) -> Result<(), JobError> {
        let specs = self.callback_specs();
        if specs.is_empty() || !store.try_claim_callbacks(&self.id).await? {
            return Ok(());
        }
        for (name, payload, idemp_key) in specs {
            // Why: attach deterministic `batch:{id}:{then|finally}` idempotency keys so
            // retrying after a partial callback enqueue failure (e.g. `then` enqueued,
            // `finally` failed, claim released) cannot duplicate `then` execution (`RC-17`).
            let raw = RawJob {
                name,
                queue: "default".to_string(),
                priority: 0,
                payload,
                retry_policy: RetryPolicy::default(),
                timeout: Some(Duration::from_secs(60)),
                idempotency_key: Some(idemp_key),
                tenant_id: None,
                trace_context: None,
            };
            if let Err(err) = queue.enqueue_raw(raw, Duration::ZERO).await {
                let _ = store.release_callbacks_claim(&self.id).await;
                return Err(err);
            }
        }
        Ok(())
    }
}

/// Persistence contract for tracking job batch progress and terminal callbacks.
pub trait BatchStore: Debug + Send + Sync + 'static {
    /// Creates and persists a new batch record.
    fn create_batch(&self, batch: JobBatch) -> BoxFuture<'_, Result<JobBatch, JobError>>;

    /// Loads a batch record by ID.
    fn load_batch(&self, id: &BatchId) -> BoxFuture<'_, Result<Option<JobBatch>, JobError>>;

    /// Atomically records a successful job completion in the batch.
    fn record_success(&self, id: &BatchId) -> BoxFuture<'_, Result<JobBatch, JobError>>;

    /// Atomically records a failed job in the batch.
    fn record_failure(&self, id: &BatchId) -> BoxFuture<'_, Result<JobBatch, JobError>>;

    /// Atomically claims the right to dispatch terminal callbacks (`true` on first claim).
    fn try_claim_callbacks(&self, id: &BatchId) -> BoxFuture<'_, Result<bool, JobError>>;

    /// Releases a previously claimed callback dispatch lock after a failed dispatch attempt
    /// so a retry can re-claim and deliver callbacks instead of dropping them permanently.
    fn release_callbacks_claim(&self, id: &BatchId) -> BoxFuture<'_, Result<(), JobError>>;

    /// Marks a running batch as cancelled.
    fn cancel(&self, id: &BatchId) -> BoxFuture<'_, Result<(), JobError>>;
}

/// In-memory [`BatchStore`] for hermetic tests and single-node deployments.
#[derive(Debug, Default)]
pub struct InMemoryBatchStore {
    batches: Mutex<HashMap<BatchId, JobBatch>>,
}

impl InMemoryBatchStore {
    /// Creates a new empty in-memory batch store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl BatchStore for InMemoryBatchStore {
    fn create_batch(&self, batch: JobBatch) -> BoxFuture<'_, Result<JobBatch, JobError>> {
        Box::pin(async move {
            let mut guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.contains_key(&batch.id) {
                return Err(JobError::Fatal(format!(
                    "batch {} already exists",
                    batch.id.0
                )));
            }
            guard.insert(batch.id.clone(), batch.clone());
            Ok(batch)
        })
    }

    fn load_batch(&self, id: &BatchId) -> BoxFuture<'_, Result<Option<JobBatch>, JobError>> {
        let id = id.clone();
        Box::pin(async move {
            let guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Ok(guard.get(&id).cloned())
        })
    }

    fn record_success(&self, id: &BatchId) -> BoxFuture<'_, Result<JobBatch, JobError>> {
        let id = id.clone();
        Box::pin(async move {
            let mut guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch = guard
                .get_mut(&id)
                .ok_or_else(|| JobError::Fatal(format!("batch {} not found", id.0)))?;
            batch.completed_jobs = batch.completed_jobs.saturating_add(1);
            batch.pending_jobs = batch.pending_jobs.saturating_sub(1);
            if batch.status != BatchStatus::Cancelled && batch.pending_jobs == 0 {
                batch.status = if batch.failed_jobs > 0 {
                    BatchStatus::Failed
                } else {
                    BatchStatus::Completed
                };
            }
            Ok(batch.clone())
        })
    }

    fn record_failure(&self, id: &BatchId) -> BoxFuture<'_, Result<JobBatch, JobError>> {
        let id = id.clone();
        Box::pin(async move {
            let mut guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch = guard
                .get_mut(&id)
                .ok_or_else(|| JobError::Fatal(format!("batch {} not found", id.0)))?;
            batch.failed_jobs = batch.failed_jobs.saturating_add(1);
            batch.pending_jobs = batch.pending_jobs.saturating_sub(1);
            if batch.status != BatchStatus::Cancelled && batch.pending_jobs == 0 {
                batch.status = BatchStatus::Failed;
            }
            Ok(batch.clone())
        })
    }

    fn try_claim_callbacks(&self, id: &BatchId) -> BoxFuture<'_, Result<bool, JobError>> {
        let id = id.clone();
        Box::pin(async move {
            let mut guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch = guard
                .get_mut(&id)
                .ok_or_else(|| JobError::Fatal(format!("batch {} not found", id.0)))?;
            if batch.callbacks_dispatched {
                Ok(false)
            } else {
                batch.callbacks_dispatched = true;
                Ok(true)
            }
        })
    }

    fn release_callbacks_claim(&self, id: &BatchId) -> BoxFuture<'_, Result<(), JobError>> {
        let id = id.clone();
        Box::pin(async move {
            let mut guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(batch) = guard.get_mut(&id) {
                batch.callbacks_dispatched = false;
            }
            Ok(())
        })
    }

    fn cancel(&self, id: &BatchId) -> BoxFuture<'_, Result<(), JobError>> {
        let id = id.clone();
        Box::pin(async move {
            let mut guard = self
                .batches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(batch) = guard.get_mut(&id)
                && batch.status == BatchStatus::Running
            {
                batch.status = BatchStatus::Cancelled;
            }
            Ok(())
        })
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
fn parse_batch_status(raw: &str) -> Result<BatchStatus, JobError> {
    match raw {
        "running" => Ok(BatchStatus::Running),
        "completed" => Ok(BatchStatus::Completed),
        "failed" => Ok(BatchStatus::Failed),
        "cancelled" => Ok(BatchStatus::Cancelled),
        other => Err(JobError::Fatal(format!("unknown batch status: {other}"))),
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
macro_rules! map_batch_row {
    ($row:expr) => {{
        use sqlx::Row;
        let id: String = $row
            .try_get(0)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let name: String = $row
            .try_get(1)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let total_jobs: i32 = $row
            .try_get(2)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let pending_jobs: i32 = $row
            .try_get(3)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let completed_jobs: i32 = $row
            .try_get(4)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let failed_jobs: i32 = $row
            .try_get(5)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let status_str: String = $row
            .try_get(6)
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let then_job_name: Option<String> = $row.try_get(7).ok();
        let then_job_payload: Option<Vec<u8>> = $row.try_get(8).ok();
        let finally_job_name: Option<String> = $row.try_get(9).ok();
        let finally_job_payload: Option<Vec<u8>> = $row.try_get(10).ok();
        let callbacks_dispatched: bool = $row.try_get(11).unwrap_or(false);

        Ok::<JobBatch, JobError>(JobBatch {
            id: BatchId(id),
            name,
            total_jobs: total_jobs.max(0) as u32,
            pending_jobs: pending_jobs.max(0) as u32,
            completed_jobs: completed_jobs.max(0) as u32,
            failed_jobs: failed_jobs.max(0) as u32,
            status: parse_batch_status(&status_str)?,
            then_job_name,
            then_job_payload,
            finally_job_name,
            finally_job_payload,
            callbacks_dispatched,
        })
    }};
}

/// SQL-backed [`BatchStore`] supporting SQLite, PostgreSQL, and MySQL.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
#[derive(Debug, Clone)]
pub enum SqlBatchStore {
    /// SQLite-backed batch store.
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::SqlitePool),
    /// PostgreSQL-backed batch store.
    #[cfg(feature = "postgres")]
    Postgres(sqlx::PgPool),
    /// MySQL-backed batch store.
    #[cfg(feature = "mysql")]
    Mysql(sqlx::MySqlPool),
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
impl SqlBatchStore {
    /// Initializes the `rume_job_batches` table for the configured SQL dialect.
    pub async fn init_schema(&self) -> Result<(), JobError> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(pool) => {
                sqlx::raw_sql(rume_data::sql::SQLITE_BATCHES_SCHEMA)
                    .execute(pool)
                    .await
                    .map_err(|e| JobError::Fatal(format!("failed to init sqlite batches: {e}")))?;
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => {
                sqlx::raw_sql(rume_data::sql::PG_BATCHES_SCHEMA)
                    .execute(pool)
                    .await
                    .map_err(|e| JobError::Fatal(format!("failed to init pg batches: {e}")))?;
            }
            #[cfg(feature = "mysql")]
            Self::Mysql(pool) => {
                sqlx::raw_sql(rume_data::sql::MYSQL_BATCHES_SCHEMA)
                    .execute(pool)
                    .await
                    .map_err(|e| JobError::Fatal(format!("failed to init mysql batches: {e}")))?;
            }
        }
        Ok(())
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
impl BatchStore for SqlBatchStore {
    fn create_batch(&self, batch: JobBatch) -> BoxFuture<'_, Result<JobBatch, JobError>> {
        Box::pin(async move {
            let total = i32::try_from(batch.total_jobs).unwrap_or(i32::MAX);
            let pending = i32::try_from(batch.pending_jobs).unwrap_or(i32::MAX);
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    sqlx::query(rume_data::sql::SQLITE_BATCHES_INSERT)
                        .bind(&batch.id.0)
                        .bind(&batch.name)
                        .bind(total)
                        .bind(pending)
                        .bind(&batch.then_job_name)
                        .bind(&batch.then_job_payload)
                        .bind(&batch.finally_job_name)
                        .bind(&batch.finally_job_payload)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    sqlx::query(rume_data::sql::PG_BATCHES_INSERT)
                        .bind(&batch.id.0)
                        .bind(&batch.name)
                        .bind(total)
                        .bind(pending)
                        .bind(&batch.then_job_name)
                        .bind(&batch.then_job_payload)
                        .bind(&batch.finally_job_name)
                        .bind(&batch.finally_job_payload)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    sqlx::query(rume_data::sql::MYSQL_BATCHES_INSERT)
                        .bind(&batch.id.0)
                        .bind(&batch.name)
                        .bind(total)
                        .bind(pending)
                        .bind(&batch.then_job_name)
                        .bind(&batch.then_job_payload)
                        .bind(&batch.finally_job_name)
                        .bind(&batch.finally_job_payload)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
            }
            Ok(batch)
        })
    }

    fn load_batch(&self, id: &BatchId) -> BoxFuture<'_, Result<Option<JobBatch>, JobError>> {
        let id = id.0.clone();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    let row = sqlx::query(rume_data::sql::SQLITE_BATCHES_LOAD)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    row.as_ref().map(|r| map_batch_row!(r)).transpose()
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    let row = sqlx::query(rume_data::sql::PG_BATCHES_LOAD)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    row.as_ref().map(|r| map_batch_row!(r)).transpose()
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    let row = sqlx::query(rume_data::sql::MYSQL_BATCHES_LOAD)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    row.as_ref().map(|r| map_batch_row!(r)).transpose()
                }
            }
        })
    }

    fn record_success(&self, id: &BatchId) -> BoxFuture<'_, Result<JobBatch, JobError>> {
        let id = id.0.clone();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    let row = sqlx::query(rume_data::sql::SQLITE_BATCHES_RECORD_SUCCESS)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .ok_or_else(|| JobError::Fatal(format!("batch {id} not found")))?;
                    map_batch_row!(&row)
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    let row = sqlx::query(rume_data::sql::PG_BATCHES_RECORD_SUCCESS)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .ok_or_else(|| JobError::Fatal(format!("batch {id} not found")))?;
                    map_batch_row!(&row)
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    let mut tx = pool
                        .begin()
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    let res = sqlx::query(rume_data::sql::MYSQL_BATCHES_RECORD_SUCCESS)
                        .bind(&id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    if res.rows_affected() == 0 {
                        return Err(JobError::Fatal(format!("batch {id} not found")));
                    }
                    let row = sqlx::query(rume_data::sql::MYSQL_BATCHES_LOAD)
                        .bind(&id)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    tx.commit()
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    map_batch_row!(&row)
                }
            }
        })
    }

    fn record_failure(&self, id: &BatchId) -> BoxFuture<'_, Result<JobBatch, JobError>> {
        let id = id.0.clone();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    let row = sqlx::query(rume_data::sql::SQLITE_BATCHES_RECORD_FAILURE)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .ok_or_else(|| JobError::Fatal(format!("batch {id} not found")))?;
                    map_batch_row!(&row)
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    let row = sqlx::query(rume_data::sql::PG_BATCHES_RECORD_FAILURE)
                        .bind(&id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .ok_or_else(|| JobError::Fatal(format!("batch {id} not found")))?;
                    map_batch_row!(&row)
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    let mut tx = pool
                        .begin()
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    let res = sqlx::query(rume_data::sql::MYSQL_BATCHES_RECORD_FAILURE)
                        .bind(&id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    if res.rows_affected() == 0 {
                        return Err(JobError::Fatal(format!("batch {id} not found")));
                    }
                    let row = sqlx::query(rume_data::sql::MYSQL_BATCHES_LOAD)
                        .bind(&id)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    tx.commit()
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                    map_batch_row!(&row)
                }
            }
        })
    }

    fn try_claim_callbacks(&self, id: &BatchId) -> BoxFuture<'_, Result<bool, JobError>> {
        let id = id.0.clone();
        Box::pin(async move {
            let affected = match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    sqlx::query(rume_data::sql::SQLITE_BATCHES_MARK_CALLBACKS_DISPATCHED)
                        .bind(&id)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .rows_affected()
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    sqlx::query(rume_data::sql::PG_BATCHES_MARK_CALLBACKS_DISPATCHED)
                        .bind(&id)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .rows_affected()
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    sqlx::query(rume_data::sql::MYSQL_BATCHES_MARK_CALLBACKS_DISPATCHED)
                        .bind(&id)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?
                        .rows_affected()
                }
            };
            Ok(affected > 0)
        })
    }

    fn release_callbacks_claim(&self, id: &BatchId) -> BoxFuture<'_, Result<(), JobError>> {
        let id = id.0.clone();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    sqlx::query(
                        "UPDATE rume_job_batches SET callbacks_dispatched = 0 WHERE id = ?1",
                    )
                    .bind(&id)
                    .execute(pool)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    sqlx::query(
                        "UPDATE rume_job_batches SET callbacks_dispatched = FALSE WHERE id = $1",
                    )
                    .bind(&id)
                    .execute(pool)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    sqlx::query(
                        "UPDATE rume_job_batches SET callbacks_dispatched = 0 WHERE id = ?",
                    )
                    .bind(&id)
                    .execute(pool)
                    .await
                    .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
            }
            Ok(())
        })
    }

    fn cancel(&self, id: &BatchId) -> BoxFuture<'_, Result<(), JobError>> {
        let id = id.0.clone();
        Box::pin(async move {
            match self {
                #[cfg(feature = "sqlite")]
                Self::Sqlite(pool) => {
                    sqlx::query(rume_data::sql::SQLITE_BATCHES_CANCEL)
                        .bind(&id)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
                #[cfg(feature = "postgres")]
                Self::Postgres(pool) => {
                    sqlx::query(rume_data::sql::PG_BATCHES_CANCEL)
                        .bind(&id)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
                #[cfg(feature = "mysql")]
                Self::Mysql(pool) => {
                    sqlx::query(rume_data::sql::MYSQL_BATCHES_CANCEL)
                        .bind(&id)
                        .execute(pool)
                        .await
                        .map_err(|e| JobError::Fatal(e.to_string()))?;
                }
            }
            Ok(())
        })
    }
}

/// Fluent builder for constructing and dispatching a [`JobBatch`].
#[derive(Debug, Default)]
pub struct BatchBuilder {
    name: String,
    jobs: Vec<RawJob>,
    then_job: Option<(String, Vec<u8>)>,
    finally_job: Option<(String, Vec<u8>)>,
    build_error: Option<String>,
}

impl BatchBuilder {
    /// Starts building a new job batch with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Returns the configured batch name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Adds a typed job to the batch.
    pub fn add_job<J: Job>(mut self, job: J) -> Self {
        match RawJob::from_job(job, None) {
            Ok(raw) => self.jobs.push(raw),
            Err(e) => {
                if self.build_error.is_none() {
                    self.build_error = Some(e.to_string());
                }
            }
        }
        self
    }

    /// Registers a `then` callback job executed only if all batch jobs succeed.
    pub fn then<J: Job>(mut self, job: J) -> Self {
        match serde_json::to_vec(&job) {
            Ok(payload) => self.then_job = Some((J::NAME.to_string(), payload)),
            Err(e) => {
                if self.build_error.is_none() {
                    self.build_error = Some(e.to_string());
                }
            }
        }
        self
    }

    /// Registers a `finally` callback job executed when the batch reaches any terminal state.
    pub fn finally<J: Job>(mut self, job: J) -> Self {
        match serde_json::to_vec(&job) {
            Ok(payload) => self.finally_job = Some((J::NAME.to_string(), payload)),
            Err(e) => {
                if self.build_error.is_none() {
                    self.build_error = Some(e.to_string());
                }
            }
        }
        self
    }

    fn build_record(self) -> Result<(JobBatch, Vec<RawJob>), JobError> {
        if let Some(err) = self.build_error {
            return Err(JobError::Serialization(err));
        }
        if self.jobs.is_empty() {
            return Err(JobError::Fatal(
                "cannot create a batch with 0 jobs".to_string(),
            ));
        }
        let total = u32::try_from(self.jobs.len()).unwrap_or(u32::MAX);
        let (then_job_name, then_job_payload) = match self.then_job {
            Some((n, p)) => (Some(n), Some(p)),
            None => (None, None),
        };
        let (finally_job_name, finally_job_payload) = match self.finally_job {
            Some((n, p)) => (Some(n), Some(p)),
            None => (None, None),
        };
        let batch = JobBatch {
            id: BatchId(Uuid::new_v4().to_string()),
            name: self.name,
            total_jobs: total,
            pending_jobs: total,
            completed_jobs: 0,
            failed_jobs: 0,
            status: BatchStatus::Running,
            then_job_name,
            then_job_payload,
            finally_job_name,
            finally_job_payload,
            callbacks_dispatched: false,
        };
        Ok((batch, self.jobs))
    }

    /// Persists the batch record in `store` without enqueueing jobs onto a queue.
    pub async fn create_in_store<S: BatchStore>(self, store: &S) -> Result<JobBatch, JobError> {
        let (batch, _) = self.build_record()?;
        store.create_batch(batch).await
    }

    /// Persists the batch and enqueues all constituent jobs.
    pub async fn dispatch<Q: JobQueue, S: BatchStore>(
        self,
        queue: &Q,
        store: &S,
    ) -> Result<JobBatch, JobError> {
        let (batch, jobs) = self.build_record()?;
        let total = jobs.len();
        let created = store.create_batch(batch).await?;
        for (idx, job) in jobs.into_iter().enumerate() {
            if let Err(err) = queue.enqueue_raw(job, Duration::ZERO).await {
                // Why: account for every job that was never enqueued so `pending_jobs`,
                // `failed_jobs`, and `status` stay accurate instead of hanging forever
                // waiting on jobs that never entered the queue.
                for _ in idx..total {
                    let _ = store.record_failure(&created.id).await;
                }
                return Err(err);
            }
        }
        Ok(created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::job::{JobContext, JobHandler};
    use crate::jobs::worker::JobRegistry;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct WorkItem {
        id: u32,
    }

    impl Job for WorkItem {
        const NAME: &'static str = "batch_work_item";
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ThenCallback {
        label: String,
    }

    impl Job for ThenCallback {
        const NAME: &'static str = "batch_then_cb";
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct FinallyCallback {
        label: String,
    }

    impl Job for FinallyCallback {
        const NAME: &'static str = "batch_finally_cb";
    }

    struct CountingHandler(Arc<AtomicUsize>);

    impl<J: Job> JobHandler<J> for CountingHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: J,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let count = Arc::clone(&self.0);
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn job_batch_fires_then_and_finally_on_all_success() {
        let store = InMemoryBatchStore::new();
        let then_count = Arc::new(AtomicUsize::new(0));
        let finally_count = Arc::new(AtomicUsize::new(0));

        let mut registry = JobRegistry::new();
        registry.register::<ThenCallback, _>(CountingHandler(Arc::clone(&then_count)));
        registry.register::<FinallyCallback, _>(CountingHandler(Arc::clone(&finally_count)));

        let batch = BatchBuilder::new("all-success")
            .add_job(WorkItem { id: 1 })
            .add_job(WorkItem { id: 2 })
            .then(ThenCallback {
                label: "done".into(),
            })
            .finally(FinallyCallback {
                label: "cleanup".into(),
            })
            .create_in_store(&store)
            .await
            .expect("create batch");

        let b1 = store.record_success(&batch.id).await.expect("success 1");
        assert_eq!(b1.status, BatchStatus::Running);
        assert_eq!(b1.pending_jobs, 1);

        let b2 = store.record_success(&batch.id).await.expect("success 2");
        assert_eq!(b2.status, BatchStatus::Completed);
        assert_eq!(b2.pending_jobs, 0);

        b2.dispatch_callbacks(&store, &registry)
            .await
            .expect("dispatch callbacks");
        // Second call must be idempotent (only one worker claims callbacks).
        b2.dispatch_callbacks(&store, &registry)
            .await
            .expect("idempotent dispatch");

        assert_eq!(then_count.load(Ordering::SeqCst), 1);
        assert_eq!(finally_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn job_batch_skips_then_and_fires_finally_on_failure() {
        let store = InMemoryBatchStore::new();
        let then_count = Arc::new(AtomicUsize::new(0));
        let finally_count = Arc::new(AtomicUsize::new(0));

        let mut registry = JobRegistry::new();
        registry.register::<ThenCallback, _>(CountingHandler(Arc::clone(&then_count)));
        registry.register::<FinallyCallback, _>(CountingHandler(Arc::clone(&finally_count)));

        let batch = BatchBuilder::new("partial-failure")
            .add_job(WorkItem { id: 1 })
            .add_job(WorkItem { id: 2 })
            .then(ThenCallback {
                label: "done".into(),
            })
            .finally(FinallyCallback {
                label: "cleanup".into(),
            })
            .create_in_store(&store)
            .await
            .expect("create batch");

        let b1 = store.record_failure(&batch.id).await.expect("failure 1");
        assert_eq!(b1.status, BatchStatus::Running);

        let b2 = store.record_success(&batch.id).await.expect("success 2");
        assert_eq!(b2.status, BatchStatus::Failed);
        assert_eq!(b2.pending_jobs, 0);

        b2.dispatch_callbacks(&store, &registry)
            .await
            .expect("dispatch callbacks");

        assert_eq!(then_count.load(Ordering::SeqCst), 0);
        assert_eq!(finally_count.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_batch_store_tracks_lifecycle_and_dispatches_callbacks_to_queue() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("sqlite memory");
        let queue = crate::jobs::SqliteJobQueue::new(pool.clone());
        queue.init_schema().await.expect("init jobs schema");
        let store = SqlBatchStore::Sqlite(pool);
        store.init_schema().await.expect("init batches schema");

        let batch = BatchBuilder::new("sql-batch")
            .add_job(WorkItem { id: 1 })
            .add_job(WorkItem { id: 2 })
            .then(ThenCallback {
                label: "sql-then".into(),
            })
            .finally(FinallyCallback {
                label: "sql-finally".into(),
            })
            .dispatch(&queue, &store)
            .await
            .expect("dispatch batch");

        assert_eq!(queue.pending_count().await.unwrap(), 2);

        let loaded = store.load_batch(&batch.id).await.unwrap().unwrap();
        assert_eq!(loaded.total_jobs, 2);
        assert_eq!(loaded.pending_jobs, 2);
        assert_eq!(loaded.status, BatchStatus::Running);

        let _ = store.record_success(&batch.id).await.unwrap();
        let done = store.record_success(&batch.id).await.unwrap();
        assert_eq!(done.status, BatchStatus::Completed);

        done.dispatch_callbacks_to_queue(&store, &queue)
            .await
            .expect("enqueue callbacks");
        // 2 original jobs + then + finally = 4 pending jobs
        assert_eq!(queue.pending_count().await.unwrap(), 4);
    }

    #[tokio::test]
    async fn empty_batch_is_rejected_instead_of_hanging_in_running_state() {
        let store = InMemoryBatchStore::new();
        let err = BatchBuilder::new("empty-batch")
            .create_in_store(&store)
            .await
            .expect_err("0-job batch must not start Running with pending_jobs = 0");
        assert!(
            err.to_string().contains("0 jobs") || err.to_string().contains("at least one"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn partial_enqueue_failure_accounts_for_unenqueued_jobs_and_allows_callback_retry() {
        use std::sync::atomic::AtomicBool;

        #[derive(Debug)]
        struct FlakyQueue {
            fail_after: usize,
            enqueued: AtomicUsize,
            fail_all: AtomicBool,
        }

        impl JobQueue for FlakyQueue {
            fn enqueue_delayed_with_trace<J: Job>(
                &self,
                job: J,
                delay: Duration,
                trace_context: Option<String>,
            ) -> BoxFuture<'_, Result<String, JobError>> {
                match RawJob::from_job(job, trace_context) {
                    Ok(raw) => self.enqueue_raw(raw, delay),
                    Err(err) => Box::pin(async move { Err(err) }),
                }
            }
            fn enqueue_raw(
                &self,
                _job: RawJob,
                _delay: Duration,
            ) -> BoxFuture<'_, Result<String, JobError>> {
                Box::pin(async move {
                    if self.fail_all.load(Ordering::SeqCst) {
                        return Err(JobError::Fatal("queue unavailable".into()));
                    }
                    let n = self.enqueued.fetch_add(1, Ordering::SeqCst);
                    if n >= self.fail_after {
                        Err(JobError::Fatal("enqueue failed mid-batch".into()))
                    } else {
                        Ok(format!("job-{n}"))
                    }
                })
            }
            fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
                Box::pin(async { Ok(None) })
            }
            fn dequeue_for(
                &self,
                _worker_id: &str,
            ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
                Box::pin(async { Ok(None) })
            }
            fn ack(&self, _job_id: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }
            fn nack(
                &self,
                _job: QueuedJob,
                _token: &str,
                _delay: Duration,
            ) -> BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }
            fn dead_letter(
                &self,
                _job: QueuedJob,
                _token: &str,
                _reason: String,
            ) -> BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }
            fn reclaim_abandoned_jobs(
                &self,
                _dead_workers: &[String],
            ) -> BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }
            fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }
            fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }
        }

        let store = InMemoryBatchStore::new();
        let queue = FlakyQueue {
            fail_after: 1,
            enqueued: AtomicUsize::new(0),
            fail_all: AtomicBool::new(false),
        };

        // 3 jobs: job 0 enqueues, job 1 fails -> jobs 1 and 2 are recorded as failed.
        let err = BatchBuilder::new("partial-enqueue")
            .add_job(WorkItem { id: 1 })
            .add_job(WorkItem { id: 2 })
            .add_job(WorkItem { id: 3 })
            .finally(FinallyCallback {
                label: "cleanup".into(),
            })
            .dispatch(&queue, &store)
            .await
            .expect_err("mid-batch enqueue failure must return Err");
        assert!(err.to_string().contains("enqueue failed mid-batch"));

        // Find the created batch and verify pending_jobs == 1, failed_jobs == 2.
        let batch = store
            .batches
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned()
            .expect("batch must be persisted");
        assert_eq!(batch.pending_jobs, 1);
        assert_eq!(batch.failed_jobs, 2);
        assert_eq!(batch.status, BatchStatus::Running);

        // When the 1 enqueued job finishes, the batch transitions cleanly to Failed.
        let terminal = store
            .record_success(&batch.id)
            .await
            .expect("record_success");
        assert_eq!(terminal.pending_jobs, 0);
        assert_eq!(terminal.status, BatchStatus::Failed);

        // Callback enqueue fails -> claim must be released so a retry succeeds.
        queue.fail_all.store(true, Ordering::SeqCst);
        let cb_err = terminal
            .dispatch_callbacks_to_queue(&store, &queue)
            .await
            .expect_err("callback enqueue must fail when queue is down");
        assert!(cb_err.to_string().contains("queue unavailable"));

        // Queue recovers; retry must re-claim and enqueue the `finally` callback.
        queue.fail_all.store(false, Ordering::SeqCst);
        queue.enqueued.store(0, Ordering::SeqCst);
        terminal
            .dispatch_callbacks_to_queue(&store, &queue)
            .await
            .expect("callback retry must succeed after claim rollback");
        assert_eq!(queue.enqueued.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn batch_callbacks_attach_deterministic_idempotency_keys() {
        #[derive(Debug, Default)]
        struct CapturingQueue {
            enqueued: Mutex<Vec<RawJob>>,
        }

        impl JobQueue for CapturingQueue {
            fn enqueue_delayed_with_trace<J: Job>(
                &self,
                job: J,
                delay: Duration,
                trace_context: Option<String>,
            ) -> BoxFuture<'_, Result<String, JobError>> {
                match RawJob::from_job(job, trace_context) {
                    Ok(raw) => self.enqueue_raw(raw, delay),
                    Err(err) => Box::pin(async move { Err(err) }),
                }
            }
            fn enqueue_raw(
                &self,
                job: RawJob,
                _delay: Duration,
            ) -> BoxFuture<'_, Result<String, JobError>> {
                Box::pin(async move {
                    let mut guard = self.enqueued.lock().unwrap();
                    let idx = guard.len();
                    guard.push(job);
                    Ok(format!("job-{idx}"))
                })
            }
            fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
                Box::pin(async { Ok(None) })
            }
            fn dequeue_for(
                &self,
                _worker_id: &str,
            ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
                Box::pin(async { Ok(None) })
            }
            fn ack(&self, _job_id: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }
            fn nack(
                &self,
                _job: QueuedJob,
                _token: &str,
                _delay: Duration,
            ) -> BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }
            fn dead_letter(
                &self,
                _job: QueuedJob,
                _token: &str,
                _reason: String,
            ) -> BoxFuture<'_, Result<(), JobError>> {
                Box::pin(async { Ok(()) })
            }
            fn reclaim_abandoned_jobs(
                &self,
                _dead_workers: &[String],
            ) -> BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }
            fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(self.enqueued.lock().unwrap().len()) })
            }
            fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
                Box::pin(async { Ok(0) })
            }
        }

        let store = InMemoryBatchStore::new();
        let queue = CapturingQueue::default();

        let batch = BatchBuilder::new("idempotent-callbacks")
            .add_job(WorkItem { id: 1 })
            .then(ThenCallback {
                label: "done".into(),
            })
            .finally(FinallyCallback {
                label: "cleanup".into(),
            })
            .create_in_store(&store)
            .await
            .expect("create batch");

        let done = store
            .record_success(&batch.id)
            .await
            .expect("record_success");
        done.dispatch_callbacks_to_queue(&store, &queue)
            .await
            .expect("dispatch callbacks");

        let jobs = queue.enqueued.lock().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].name, ThenCallback::NAME);
        assert_eq!(
            jobs[0].idempotency_key.as_deref(),
            Some(format!("batch:{}:then", batch.id.0).as_str())
        );
        assert_eq!(jobs[1].name, FinallyCallback::NAME);
        assert_eq!(
            jobs[1].idempotency_key.as_deref(),
            Some(format!("batch:{}:finally", batch.id.0).as_str())
        );
    }
}
