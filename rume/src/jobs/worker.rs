//! Background worker runner, handler registry, and graceful drain lifecycle.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::sleep;
use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};
use crate::jobs::queue::JobQueue;
use crate::jobs::reliability::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, scoped_idempotency_key,
};

type ErasedHandler =
    Arc<dyn Fn(&JobContext, &[u8]) -> BoxFuture<'static, Result<(), JobError>> + Send + Sync>;

/// Registry mapping job names to their respective typed execution handlers.
#[derive(Default, Clone)]
pub struct JobRegistry {
    handlers: BTreeMap<String, ErasedHandler>,
}

impl Debug for JobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRegistry")
            .field("registered_jobs", &self.handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl JobRegistry {
    /// Creates an empty job registry.
    pub fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
        }
    }

    /// Registers a typed handler for a job type.
    pub fn register<J, H>(&mut self, handler: H)
    where
        J: Job,
        H: JobHandler<J>,
    {
        let handler = Arc::new(handler);
        let erased: ErasedHandler = Arc::new(move |ctx, payload| {
            let handler = Arc::clone(&handler);
            let ctx = ctx.clone();
            let payload = payload.to_vec();

            Box::pin(async move {
                let job: J = serde_json::from_slice(&payload)
                    .map_err(|e| JobError::Serialization(e.to_string()))?;
                handler.handle(&ctx, job).await
            })
        });

        self.handlers.insert(J::NAME.to_string(), erased);
    }

    /// Dispatches a raw queued job to its registered handler.
    pub async fn dispatch(&self, job: &QueuedJob) -> Result<(), JobError> {
        self.dispatch_with_cancellation(job, None).await
    }

    /// Dispatches a raw queued job to its registered handler with optional cancellation awareness.
    pub async fn dispatch_with_cancellation(
        &self,
        job: &QueuedJob,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> Result<(), JobError> {
        let Some(handler) = self.handlers.get(&job.name) else {
            return Err(JobError::UnknownJob(job.name.clone()));
        };

        let ctx = JobContext {
            id: job.id.clone(),
            attempt: job.attempt,
            max_attempts: job.retry_policy.max_attempts,
            cancellation,
        };

        let fut = handler(&ctx, &job.payload);
        if let Some(timeout) = job.timeout {
            match tokio::time::timeout(timeout, fut).await {
                Ok(res) => res,
                Err(_) => Err(JobError::Timeout),
            }
        } else {
            fut.await
        }
    }
}

/// RAII drop guard ensuring in-flight job counter is decremented even if the worker panics.
struct InFlightGuard(Arc<AtomicU32>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Background worker managing queue polling, retries, and graceful shutdown drain.
pub struct JobWorker<Q: JobQueue> {
    worker_id: String,
    queue: Arc<Q>,
    registry: Arc<JobRegistry>,
    is_stopping: Arc<AtomicBool>,
    in_flight_count: Arc<AtomicU32>,
    concurrency_limit: usize,
    semaphore: Arc<tokio::sync::Semaphore>,
    heartbeat_registry: Option<Arc<dyn HeartbeatRegistry>>,
    heartbeat_ttl: Duration,
    heartbeat_interval: Duration,
    abandoned_recovery_interval: Option<Duration>,
    idempotency_store: Option<Arc<dyn IdempotencyStore>>,
}

impl<Q: JobQueue> Clone for JobWorker<Q> {
    fn clone(&self) -> Self {
        Self {
            worker_id: self.worker_id.clone(),
            queue: Arc::clone(&self.queue),
            registry: Arc::clone(&self.registry),
            is_stopping: Arc::clone(&self.is_stopping),
            in_flight_count: Arc::clone(&self.in_flight_count),
            concurrency_limit: self.concurrency_limit,
            semaphore: Arc::clone(&self.semaphore),
            heartbeat_registry: self.heartbeat_registry.clone(),
            heartbeat_ttl: self.heartbeat_ttl,
            heartbeat_interval: self.heartbeat_interval,
            abandoned_recovery_interval: self.abandoned_recovery_interval,
            idempotency_store: self.idempotency_store.clone(),
        }
    }
}

impl<Q: JobQueue> Debug for JobWorker<Q> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobWorker")
            .field("worker_id", &self.worker_id)
            .field("is_stopping", &self.is_stopping.load(Ordering::Relaxed))
            .field(
                "in_flight_jobs",
                &self.in_flight_count.load(Ordering::Relaxed),
            )
            .field("concurrency_limit", &self.concurrency_limit)
            .finish()
    }
}

impl<Q: JobQueue> JobWorker<Q> {
    /// Default concurrent job execution limit.
    pub const DEFAULT_CONCURRENCY: usize = 50;

    /// Creates a new background worker with default concurrency limit.
    pub fn new(queue: Arc<Q>, registry: Arc<JobRegistry>) -> Self {
        Self::with_concurrency_limit(queue, registry, Self::DEFAULT_CONCURRENCY)
    }

    /// Creates a new background worker with a custom concurrency limit.
    pub fn with_concurrency_limit(
        queue: Arc<Q>,
        registry: Arc<JobRegistry>,
        concurrency_limit: usize,
    ) -> Self {
        let limit = concurrency_limit.max(1);
        Self {
            worker_id: format!("worker-{}", Uuid::new_v4()),
            queue,
            registry,
            is_stopping: Arc::new(AtomicBool::new(false)),
            in_flight_count: Arc::new(AtomicU32::new(0)),
            concurrency_limit: limit,
            semaphore: Arc::new(tokio::sync::Semaphore::new(limit)),
            heartbeat_registry: None,
            heartbeat_ttl: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(10),
            abandoned_recovery_interval: None,
            idempotency_store: None,
        }
    }

    /// Sets the worker's concurrent task capacity.
    pub fn with_concurrency(mut self, limit: usize) -> Self {
        let limit = limit.max(1);
        self.concurrency_limit = limit;
        self.semaphore = Arc::new(tokio::sync::Semaphore::new(limit));
        self
    }

    /// Sets a custom unique worker identifier.
    pub fn with_worker_id(mut self, id: impl Into<String>) -> Self {
        self.worker_id = id.into();
        self
    }

    /// Configures heartbeat registration with custom TTL and renewal interval.
    pub fn with_heartbeat(
        mut self,
        registry: Arc<dyn HeartbeatRegistry>,
        ttl: Duration,
        interval: Duration,
    ) -> Self {
        self.heartbeat_registry = Some(registry);
        self.heartbeat_ttl = ttl;
        self.heartbeat_interval = interval;
        self
    }

    /// Configures background abandoned job recovery running at the specified interval.
    pub fn with_abandoned_recovery(mut self, interval: Duration) -> Self {
        self.abandoned_recovery_interval = Some(interval);
        self
    }

    /// Configures an idempotency store for deduplicating execution.
    pub fn with_idempotency_store(mut self, store: Arc<dyn IdempotencyStore>) -> Self {
        self.idempotency_store = Some(store);
        self
    }

    /// Returns the unique worker identifier.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Spawns the background execution loop into the current async runtime.
    pub fn start(&self) {
        let worker = self.clone();
        tokio::spawn(async move {
            worker.run_loop().await;
        });

        if let Some(heartbeat) = self.heartbeat_registry.clone() {
            let worker_id = self.worker_id.clone();
            let is_stopping = Arc::clone(&self.is_stopping);
            let ttl = self.heartbeat_ttl;
            let interval = self.heartbeat_interval;
            let hb_loop = Arc::clone(&heartbeat);

            tokio::spawn(async move {
                while !is_stopping.load(Ordering::Relaxed) {
                    let _ = hb_loop.record_heartbeat(&worker_id, ttl).await;
                    sleep(interval).await;
                }
            });

            if let Some(recovery_interval) = self.abandoned_recovery_interval {
                let queue = Arc::clone(&self.queue);
                let is_stopping = Arc::clone(&self.is_stopping);
                let hb_reap = Arc::clone(&heartbeat);
                tokio::spawn(async move {
                    while !is_stopping.load(Ordering::Relaxed) {
                        sleep(recovery_interval).await;
                        if let Ok(dead) = hb_reap.reap_dead_workers().await
                            && !dead.is_empty()
                        {
                            let _ = queue.reclaim_abandoned_jobs(&dead).await;
                        }
                    }
                });
            }
        }
    }

    /// Internal worker execution loop.
    async fn run_loop(&self) {
        let mut consecutive_errors: u32 = 0;
        while !self.is_stopping.load(Ordering::Relaxed) {
            let permit = match self.semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => break,
            };

            match self.queue.dequeue_for(&self.worker_id).await {
                Ok(Some(job)) => {
                    consecutive_errors = 0;
                    self.in_flight_count.fetch_add(1, Ordering::SeqCst);
                    let queue = Arc::clone(&self.queue);
                    let registry = Arc::clone(&self.registry);
                    let in_flight = Arc::clone(&self.in_flight_count);
                    let idempotency = self.idempotency_store.clone();
                    let stopping = Arc::clone(&self.is_stopping);

                    tokio::spawn(async move {
                        let _permit = permit;
                        let _guard = InFlightGuard(in_flight);
                        Self::execute_job(queue, registry, job, idempotency, stopping).await;
                    });
                }
                Ok(None) => {
                    consecutive_errors = 0;
                    drop(permit);
                    sleep(Duration::from_millis(25)).await;
                }
                Err(_) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    drop(permit);
                    let backoff_ms = (50u64 * 2u64.pow(consecutive_errors.min(6))).min(5000);
                    sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    /// Executes a single dequeued job, handling acknowledgments, retries, and dead-lettering.
    async fn execute_job(
        queue: Arc<Q>,
        registry: Arc<JobRegistry>,
        job: QueuedJob,
        idempotency_store: Option<Arc<dyn IdempotencyStore>>,
        stopping: Arc<AtomicBool>,
    ) {
        let job_id = job.id.clone();
        let token = job.lease_token.clone().unwrap_or_default();

        let effective_key = match (&job.tenant_id, &job.idempotency_key) {
            (Some(tenant), Some(key)) => Some(scoped_idempotency_key(tenant, key)),
            (None, Some(key)) => Some(key.clone()),
            _ => None,
        };

        if let (Some(store), Some(key)) = (&idempotency_store, &effective_key) {
            match store
                .try_acquire_with_token(key, &token, Duration::from_secs(300))
                .await
            {
                Ok(IdempotencyStatus::Completed(_)) => {
                    let _ = queue.ack(&job_id, &token).await;
                    return;
                }
                Ok(IdempotencyStatus::InProgress) => {
                    let _ = queue.nack(job, &token, Duration::from_secs(2)).await;
                    return;
                }
                _ => {}
            }
        }

        // Spawn background lease heartbeat while job is executing
        let job_cancelled = Arc::new(AtomicBool::new(stopping.load(Ordering::Relaxed)));
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let hb_token = token.clone();
        let hb_queue = Arc::clone(&queue);
        let hb_jid = job_id.clone();
        let hb_cancelled = Arc::clone(&job_cancelled);
        let hb_stopping = Arc::clone(&stopping);

        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(20));
            interval.tick().await; // tick immediately
            let mut stop_check = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => break,
                    _ = stop_check.tick() => {
                        if hb_stopping.load(Ordering::Relaxed) {
                            hb_cancelled.store(true, Ordering::SeqCst);
                        }
                    }
                    _ = interval.tick() => {
                        if let Err(e) = hb_queue.renew_lease(&hb_jid, &hb_token, Duration::from_secs(60)).await {
                            tracing::warn!(job_id = %hb_jid, error = %e, "Failed to renew job lease heartbeat");
                            hb_cancelled.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
        });

        let dispatch_result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.dispatch_with_cancellation(&job, Some(job_cancelled))
        })) {
            Ok(fut) => match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
                Ok(res) => res,
                Err(_) => Err(JobError::Fatal("handler panicked".into())),
            },
            Err(_) => Err(JobError::Fatal("handler panicked".into())),
        };

        let _ = cancel_tx.send(());
        heartbeat_handle.abort();

        match dispatch_result {
            Ok(()) => {
                let _ = queue.ack(&job_id, &token).await;
                if let (Some(store), Some(key)) = (&idempotency_store, &effective_key) {
                    let _ = store.complete_with_token(key, &token, None).await;
                }
            }
            Err(JobError::Fatal(msg))
            | Err(JobError::UnknownJob(msg))
            | Err(JobError::Serialization(msg)) => {
                if let (Some(store), Some(key)) = (&idempotency_store, &effective_key) {
                    let _ = store.release_with_token(key, &token).await;
                }
                let _ = queue.dead_letter(job, &token, msg).await;
            }
            Err(err) => {
                // Retryable failure: check remaining attempts
                if let Some(backoff) = job.retry_policy.next_backoff(job.attempt) {
                    let _ = queue.nack(job, &token, backoff).await;
                } else {
                    if let (Some(store), Some(key)) = (&idempotency_store, &effective_key) {
                        let _ = store.release_with_token(key, &token).await;
                    }
                    let reason = format!("max retries exceeded ({}) - error: {err}", job.attempt);
                    let _ = queue.dead_letter(job, &token, reason).await;
                }
            }
        }
    }

    /// Gracefully requests worker shutdown and waits for in-flight jobs to complete within `timeout`.
    pub async fn drain(&self, timeout: Duration) -> bool {
        self.is_stopping.store(true, Ordering::SeqCst);
        let start = std::time::Instant::now();

        while self.in_flight_count.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= timeout {
                return false;
            }
            sleep(Duration::from_millis(20)).await;
        }

        true
    }

    /// Returns the number of currently executing jobs.
    pub fn in_flight_count(&self) -> u32 {
        self.in_flight_count.load(Ordering::Relaxed)
    }
}
