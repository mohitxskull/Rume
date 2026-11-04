//! Background worker runner, handler registry, and graceful drain lifecycle.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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

/// Runtime status summary of a background job worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStatus {
    /// Unique worker identifier.
    pub worker_id: String,
    /// Number of jobs currently being executed.
    pub in_flight: u32,
    /// Worker concurrency limit.
    pub concurrency_limit: usize,
    /// Whether the worker is currently shutting down.
    pub is_stopping: bool,
    /// Number of failed job state transitions (ack, nack, dead_letter).
    pub transition_errors: u64,
    /// Number of lost leases detected during heartbeat renewal or completion.
    pub lost_leases: u64,
}

/// Handle to background tasks spawned by [`JobWorker::start`].
///
/// Enables external supervision, explicit task inspection, and graceful awaitable shutdown.
#[derive(Debug)]
pub struct WorkerHandle {
    /// Join handle of the primary queue polling and execution loop.
    pub worker_task: tokio::task::JoinHandle<()>,
    /// Join handle of the worker heartbeat registration loop, if configured.
    pub heartbeat_task: Option<tokio::task::JoinHandle<()>>,
    /// Join handle of the dead worker abandoned job recovery loop, if configured.
    pub recovery_task: Option<tokio::task::JoinHandle<()>>,
    is_stopping: Arc<AtomicBool>,
    shutdown_notify: Arc<tokio::sync::Notify>,
    in_flight_count: Arc<AtomicU32>,
    active_tasks: Arc<AtomicU32>,
}

impl WorkerHandle {
    /// Returns the number of currently running worker background tasks (execution, heartbeat, recovery).
    pub fn active_tasks(&self) -> u32 {
        self.active_tasks.load(Ordering::Relaxed)
    }

    /// Returns `true` if the main polling task and all auxiliary tasks have completed.
    pub fn is_finished(&self) -> bool {
        self.worker_task.is_finished()
            && self.heartbeat_task.as_ref().is_none_or(|h| h.is_finished())
            && self.recovery_task.as_ref().is_none_or(|h| h.is_finished())
    }

    /// Aborts all worker and auxiliary background tasks immediately.
    pub fn abort_all(&self) {
        self.is_stopping.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        self.worker_task.abort();
        if let Some(h) = &self.heartbeat_task {
            h.abort();
        }
        if let Some(r) = &self.recovery_task {
            r.abort();
        }
    }

    /// Signals the worker to stop, waits for in-flight jobs to complete, and awaits all tasks.
    ///
    /// Returns `true` if cleanly shut down within `timeout`, or `false` if jobs or tasks remained.
    pub async fn shutdown(mut self, timeout: Duration) -> bool {
        self.is_stopping.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        let start = std::time::Instant::now();

        // 1. Wait for in-flight job handlers to complete
        while self.in_flight_count.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // 2. Await the worker execution loop
        let remaining = timeout.saturating_sub(start.elapsed());
        if !await_supervised_task(&mut self.worker_task, remaining, "worker_execution").await {
            return false;
        }

        // 3. Await heartbeat task if present
        if let Some(mut hb) = self.heartbeat_task.take() {
            let remaining = timeout.saturating_sub(start.elapsed());
            if !await_supervised_task(&mut hb, remaining, "worker_heartbeat").await {
                return false;
            }
        }

        // 4. Await recovery task if present
        if let Some(mut rec) = self.recovery_task.take() {
            let remaining = timeout.saturating_sub(start.elapsed());
            if !await_supervised_task(&mut rec, remaining, "worker_recovery").await {
                return false;
            }
        }

        true
    }
}

async fn await_supervised_task(
    task: &mut tokio::task::JoinHandle<()>,
    remaining: Duration,
    task_name: &'static str,
) -> bool {
    match tokio::time::timeout(remaining, task).await {
        Ok(Ok(())) => true,
        Ok(Err(join_err)) => {
            if join_err.is_panic() {
                tracing::error!(task = task_name, "Worker background task panicked during execution");
            } else if join_err.is_cancelled() {
                tracing::warn!(task = task_name, "Worker background task was cancelled");
            } else {
                tracing::error!(task = task_name, "Worker background task failed: {join_err}");
            }
            false
        }
        Err(_) => {
            tracing::warn!(task = task_name, "Worker background task shutdown timed out");
            false
        }
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
    transition_errors: Arc<AtomicU64>,
    lost_leases: Arc<AtomicU64>,
    shutdown_notify: Arc<tokio::sync::Notify>,
    active_tasks: Arc<AtomicU32>,
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
            transition_errors: Arc::clone(&self.transition_errors),
            lost_leases: Arc::clone(&self.lost_leases),
            shutdown_notify: Arc::clone(&self.shutdown_notify),
            active_tasks: Arc::clone(&self.active_tasks),
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
            .field(
                "transition_errors",
                &self.transition_errors.load(Ordering::Relaxed),
            )
            .field("lost_leases", &self.lost_leases.load(Ordering::Relaxed))
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
            transition_errors: Arc::new(AtomicU64::new(0)),
            lost_leases: Arc::new(AtomicU64::new(0)),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
            active_tasks: Arc::new(AtomicU32::new(0)),
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

    /// Returns the current runtime status of the worker.
    pub fn status(&self) -> WorkerStatus {
        WorkerStatus {
            worker_id: self.worker_id.clone(),
            in_flight: self.in_flight_count.load(Ordering::Relaxed),
            concurrency_limit: self.concurrency_limit,
            is_stopping: self.is_stopping.load(Ordering::Relaxed),
            transition_errors: self.transition_errors.load(Ordering::Relaxed),
            lost_leases: self.lost_leases.load(Ordering::Relaxed),
        }
    }

    /// Returns the total count of failed state transitions (ack/nack/dead-letter).
    pub fn transition_errors(&self) -> u64 {
        self.transition_errors.load(Ordering::Relaxed)
    }

    /// Returns the total count of lost leases detected during heartbeat renewal or completion.
    pub fn lost_leases(&self) -> u64 {
        self.lost_leases.load(Ordering::Relaxed)
    }

    /// Spawns the background execution loop into the current async runtime.
    ///
    /// Returns a [`WorkerHandle`] providing lifecycle supervision, status queries,
    /// and graceful awaitable shutdown across execution, heartbeat, and recovery tasks.
    pub fn start(&self) -> WorkerHandle {
        self.active_tasks.fetch_add(1, Ordering::SeqCst);
        let worker = self.clone();
        let worker_task = tokio::spawn(async move {
            worker.run_loop().await;
            worker.active_tasks.fetch_sub(1, Ordering::SeqCst);
        });

        let mut heartbeat_task = None;
        let mut recovery_task = None;

        if let Some(heartbeat) = self.heartbeat_registry.clone() {
            let worker_id = self.worker_id.clone();
            let is_stopping = Arc::clone(&self.is_stopping);
            let shutdown_notify = Arc::clone(&self.shutdown_notify);
            let active_tasks = Arc::clone(&self.active_tasks);
            let ttl = self.heartbeat_ttl;
            let interval = self.heartbeat_interval;
            let hb_loop = Arc::clone(&heartbeat);

            active_tasks.fetch_add(1, Ordering::SeqCst);
            heartbeat_task = Some(tokio::spawn(async move {
                while !is_stopping.load(Ordering::SeqCst) {
                    if let Err(e) = hb_loop.record_heartbeat(&worker_id, ttl).await {
                        tracing::warn!(worker_id = %worker_id, error = %e, "Failed to record worker heartbeat");
                    }
                    tokio::select! {
                        _ = sleep(interval) => {},
                        _ = shutdown_notify.notified() => { break; }
                    }
                }
                active_tasks.fetch_sub(1, Ordering::SeqCst);
            }));

            if let Some(recovery_interval) = self.abandoned_recovery_interval {
                let queue = Arc::clone(&self.queue);
                let is_stopping = Arc::clone(&self.is_stopping);
                let shutdown_notify = Arc::clone(&self.shutdown_notify);
                let active_tasks = Arc::clone(&self.active_tasks);
                let hb_reap = Arc::clone(&heartbeat);

                active_tasks.fetch_add(1, Ordering::SeqCst);
                recovery_task = Some(tokio::spawn(async move {
                    while !is_stopping.load(Ordering::SeqCst) {
                        tokio::select! {
                            _ = sleep(recovery_interval) => {},
                            _ = shutdown_notify.notified() => { break; }
                        }
                        if is_stopping.load(Ordering::SeqCst) {
                            break;
                        }
                        match hb_reap.reap_dead_workers().await {
                            Ok(dead) => {
                                if !dead.is_empty()
                                    && let Err(e) = queue.reclaim_abandoned_jobs(&dead).await
                                {
                                    tracing::error!(
                                        error = %e,
                                        dead_workers = ?dead,
                                        "Failed to reclaim abandoned jobs"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to reap dead workers during recovery");
                            }
                        }
                    }
                    active_tasks.fetch_sub(1, Ordering::SeqCst);
                }));
            }
        }

        WorkerHandle {
            worker_task,
            heartbeat_task,
            recovery_task,
            is_stopping: Arc::clone(&self.is_stopping),
            shutdown_notify: Arc::clone(&self.shutdown_notify),
            in_flight_count: Arc::clone(&self.in_flight_count),
            active_tasks: Arc::clone(&self.active_tasks),
        }
    }

    /// Internal worker execution loop.
    async fn run_loop(&self) {
        let mut consecutive_errors: u32 = 0;
        while !self.is_stopping.load(Ordering::SeqCst) {
            let permit = tokio::select! {
                res = self.semaphore.clone().acquire_owned() => match res {
                    Ok(permit) => permit,
                    Err(_) => break,
                },
                _ = self.shutdown_notify.notified() => break,
            };

            if self.is_stopping.load(Ordering::SeqCst) {
                drop(permit);
                break;
            }

            match self.queue.dequeue_for(&self.worker_id).await {
                Ok(Some(job)) => {
                    consecutive_errors = 0;
                    if self.is_stopping.load(Ordering::SeqCst) {
                        // Crucial race check: draining started while dequeue was in-flight!
                        // Guarantee that no new job starts after draining begins.
                        let token = job.lease_token.clone().unwrap_or_default();
                        let _ = self.queue.nack(job, &token, Duration::ZERO).await;
                        drop(permit);
                        break;
                    }

                    self.in_flight_count.fetch_add(1, Ordering::SeqCst);
                    let queue = Arc::clone(&self.queue);
                    let registry = Arc::clone(&self.registry);
                    let in_flight = Arc::clone(&self.in_flight_count);
                    let idempotency = self.idempotency_store.clone();
                    let stopping = Arc::clone(&self.is_stopping);
                    let transition_errors = Arc::clone(&self.transition_errors);
                    let lost_leases = Arc::clone(&self.lost_leases);

                    tokio::spawn(async move {
                        let _permit = permit;
                        let _guard = InFlightGuard(in_flight);
                        Self::execute_job(
                            queue,
                            registry,
                            job,
                            idempotency,
                            stopping,
                            transition_errors,
                            lost_leases,
                        )
                        .await;
                    });
                }
                Ok(None) => {
                    consecutive_errors = 0;
                    drop(permit);
                    tokio::select! {
                        _ = sleep(Duration::from_millis(25)) => {},
                        _ = self.shutdown_notify.notified() => { break; }
                    }
                }
                Err(_) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    drop(permit);
                    let backoff_ms = (50u64 * 2u64.pow(consecutive_errors.min(6))).min(5000);
                    tokio::select! {
                        _ = sleep(Duration::from_millis(backoff_ms)) => {},
                        _ = self.shutdown_notify.notified() => { break; }
                    }
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
        transition_errors: Arc<AtomicU64>,
        lost_leases: Arc<AtomicU64>,
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
                    if let Err(err) = queue.ack(&job_id, &token).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %err,
                            "Failed to ack idempotent completed job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    }
                    return;
                }
                Ok(IdempotencyStatus::InProgress) => {
                    if let Err(err) = queue.nack(job, &token, Duration::from_secs(2)).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %err,
                            "Failed to nack in-progress idempotent job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    }
                    return;
                }
                Ok(IdempotencyStatus::New) => {}
                Err(err) => {
                    tracing::error!(
                        job_id = %job_id,
                        key = %key,
                        error = %err,
                        "Failed to acquire idempotency lock"
                    );
                }
            }
        }

        // Derive heartbeat renewal timing directly from configured lease duration
        let lease_duration = job.lease_duration;
        let hb_interval = (lease_duration / 3)
            .max(Duration::from_millis(20))
            .min(Duration::from_secs(20));
        let renew_extension = lease_duration.max(Duration::from_secs(60));

        // Spawn background lease heartbeat while job is executing
        let job_cancelled = Arc::new(AtomicBool::new(stopping.load(Ordering::Relaxed)));
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let hb_token = token.clone();
        let hb_queue = Arc::clone(&queue);
        let hb_jid = job_id.clone();
        let hb_cancelled = Arc::clone(&job_cancelled);
        let hb_stopping = Arc::clone(&stopping);
        let hb_lost_leases = Arc::clone(&lost_leases);

        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(hb_interval);
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
                        if let Err(e) = hb_queue.renew_lease(&hb_jid, &hb_token, renew_extension).await {
                            tracing::error!(job_id = %hb_jid, error = %e, "Failed to renew job lease heartbeat; lost lease");
                            hb_cancelled.store(true, Ordering::SeqCst);
                            hb_lost_leases.fetch_add(1, Ordering::SeqCst);
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
                if let Err(err) = queue.ack(&job_id, &token).await {
                    tracing::error!(
                        job_id = %job_id,
                        error = %err,
                        "Failed to ack job; lease may have expired or state mismatch"
                    );
                    transition_errors.fetch_add(1, Ordering::SeqCst);
                    lost_leases.fetch_add(1, Ordering::SeqCst);
                }
                if let (Some(store), Some(key)) = (&idempotency_store, &effective_key)
                    && let Err(err) = store.complete_with_token(key, &token, None).await
                {
                    tracing::error!(
                        job_id = %job_id,
                        key = %key,
                        error = %err,
                        "Failed to record idempotency completion"
                    );
                }
            }
            Err(JobError::Fatal(msg))
            | Err(JobError::UnknownJob(msg))
            | Err(JobError::Serialization(msg)) => {
                if let (Some(store), Some(key)) = (&idempotency_store, &effective_key)
                    && let Err(err) = store.release_with_token(key, &token).await
                {
                    tracing::error!(
                        job_id = %job_id,
                        key = %key,
                        error = %err,
                        "Failed to release idempotency token on fatal failure"
                    );
                }
                if let Err(err) = queue.dead_letter(job, &token, msg).await {
                    tracing::error!(
                        job_id = %job_id,
                        error = %err,
                        "Failed to dead-letter job; lease may have expired or state mismatch"
                    );
                    transition_errors.fetch_add(1, Ordering::SeqCst);
                    lost_leases.fetch_add(1, Ordering::SeqCst);
                }
            }
            Err(err) => {
                // Retryable failure: check remaining attempts.
                // Do not release the idempotency lock during retry backoff; ownership is retained
                // until terminal transition (success via ack or terminal failure to dead_letter).
                if let Some(backoff) = job.retry_policy.next_backoff(job.attempt) {
                    if let Err(nack_err) = queue.nack(job, &token, backoff).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %nack_err,
                            "Failed to nack job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    }
                } else {
                    if let (Some(store), Some(key)) = (&idempotency_store, &effective_key)
                        && let Err(rel_err) = store.release_with_token(key, &token).await
                    {
                        tracing::error!(
                            job_id = %job_id,
                            key = %key,
                            error = %rel_err,
                            "Failed to release idempotency token on dead letter"
                        );
                    }
                    let reason = format!("max retries exceeded ({}) - error: {err}", job.attempt);
                    if let Err(dl_err) = queue.dead_letter(job, &token, reason).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %dl_err,
                            "Failed to dead-letter job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    /// Gracefully requests worker shutdown and waits for in-flight jobs and background polling tasks to complete within `timeout`.
    pub async fn drain(&self, timeout: Duration) -> bool {
        self.is_stopping.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
        let start = std::time::Instant::now();

        // 1. Wait for in-flight jobs to finish
        while self.in_flight_count.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= timeout {
                return false;
            }
            sleep(Duration::from_millis(10)).await;
        }

        // 2. Wait for polling, heartbeat, and recovery loops to exit
        while self.active_tasks.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= timeout {
                return false;
            }
            sleep(Duration::from_millis(10)).await;
        }

        true
    }

    /// Returns the number of currently executing jobs.
    pub fn in_flight_count(&self) -> u32 {
        self.in_flight_count.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    use tokio::sync::{mpsc, Mutex};
    use crate::jobs::job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};
    use crate::jobs::retry::RetryPolicy;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct BlockTestJob {
        data: String,
    }

    impl Job for BlockTestJob {
        const NAME: &'static str = "block_test_job";
    }

    struct BlockTestHandler {
        executions: Arc<AtomicUsize>,
    }

    impl JobHandler<BlockTestJob> for BlockTestHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: BlockTestJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let executions = Arc::clone(&self.executions);
            Box::pin(async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[derive(Debug)]
    struct BlockingTestQueue {
        job: Mutex<Option<QueuedJob>>,
        enter_tx: mpsc::Sender<()>,
        release_rx: Mutex<mpsc::Receiver<()>>,
        nack_count: Arc<AtomicUsize>,
    }

    impl JobQueue for BlockingTestQueue {
        fn enqueue_delayed<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async move { Ok("test-id".to_string()) })
        }

        fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async move { Ok(None) })
        }

        fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            let worker_id = worker_id.to_string();
            Box::pin(async move {
                let _ = self.enter_tx.send(()).await;
                let _ = self.release_rx.lock().await.recv().await;
                let mut guard = self.job.lock().await;
                if let Some(mut j) = guard.take() {
                    j.assigned_worker = Some(worker_id);
                    j.lease_token = Some("test-token".to_string());
                    Ok(Some(j))
                } else {
                    Ok(None)
                }
            })
        }

        fn reclaim_abandoned_jobs(
            &self,
            _dead_workers: &[String],
        ) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async move { Ok(0) })
        }

        fn ack(&self, _job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn nack(
            &self,
            job: QueuedJob,
            _lease_token: &str,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            self.nack_count.fetch_add(1, Ordering::SeqCst);
            let mut guard = self.job.try_lock().unwrap();
            *guard = Some(job);
            Box::pin(async move { Ok(()) })
        }

        fn dead_letter(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async move { Ok(0) })
        }

        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async move { Ok(0) })
        }
    }

    #[tokio::test]
    async fn worker_drain_while_dequeue_blocked_does_not_start_job_and_nacks_immediately() {
        let (enter_tx, mut enter_rx) = mpsc::channel(1);
        let (release_tx, release_rx) = mpsc::channel(1);
        let nack_count = Arc::new(AtomicUsize::new(0));

        let payload_bytes = serde_json::to_vec(&BlockTestJob {
            data: "hello".into(),
        })
        .unwrap();

        let job = QueuedJob {
            id: "job-123".to_string(),
            name: BlockTestJob::NAME.to_string(),
            payload: payload_bytes,
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(10)),
            idempotency_key: None,
            tenant_id: None,
            assigned_worker: None,
            lease_token: None,
            available_at: Instant::now(),
            lease_duration: Duration::from_secs(10),
        };

        let queue = Arc::new(BlockingTestQueue {
            job: Mutex::new(Some(job)),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::clone(&nack_count),
        });

        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = JobRegistry::new();
        registry.register(BlockTestHandler {
            executions: Arc::clone(&executions),
        });

        let worker = JobWorker::new(queue.clone(), Arc::new(registry));
        let handle = worker.start();

        // 1. Wait until the worker has entered dequeue_for and is actively suspended
        enter_rx.recv().await.expect("worker entered dequeue_for");

        // 2. Concurrently initiate drain
        let drain_worker = worker.clone();
        let drain_task = tokio::spawn(async move {
            drain_worker.drain(Duration::from_secs(2)).await
        });

        // Small yield so drain sets is_stopping
        tokio::time::sleep(Duration::from_millis(20)).await;

        // 3. Release dequeue_for so it returns the job
        release_tx.send(()).await.expect("released dequeue");

        // 4. Drain should complete cleanly and return true
        let drained = drain_task.await.expect("drain task");
        assert!(drained, "worker should drain cleanly within timeout");

        // 5. Verify task handle is finished
        assert!(handle.is_finished());

        // 6. Guarantee that no new job started after draining began!
        assert_eq!(executions.load(Ordering::SeqCst), 0, "job must not execute after drain requested");

        // 7. Verify the returned job was immediately nack'd back to queue
        assert_eq!(nack_count.load(Ordering::SeqCst), 1, "unstarted job must be nack'd immediately");
    }

    #[tokio::test]
    async fn test_worker_handle_shutdown_reports_panic() {
        let is_stopping = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(tokio::sync::Notify::new());
        let in_flight_count = Arc::new(AtomicU32::new(0));
        let active_tasks = Arc::new(AtomicU32::new(1));

        let panicked_task = tokio::spawn(async {
            panic!("intentional test panic in worker task");
        });

        let handle = WorkerHandle {
            worker_task: panicked_task,
            heartbeat_task: None,
            recovery_task: None,
            is_stopping,
            shutdown_notify,
            in_flight_count,
            active_tasks,
        };

        let success = handle.shutdown(Duration::from_secs(1)).await;
        assert!(!success, "WorkerHandle::shutdown must return false when a task panics");
    }
}
