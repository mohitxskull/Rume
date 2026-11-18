//! Background worker runner, handler registry, and graceful drain lifecycle.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use tokio::time::sleep;
use tracing::Instrument;
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
            trace_context: job.trace_context.clone(),
        };

        let tc = job
            .trace_context
            .as_deref()
            .and_then(|s| {
                rume_observability::TraceContext::parse_serialized(s).map(|c| c.new_child())
            })
            .unwrap_or_else(rume_observability::TraceContext::generate);

        let span = tracing::info_span!(
            "job.dispatch",
            job.id = %job.id,
            job.name = %job.name,
            job.attempt = job.attempt,
            traceparent = %tc.traceparent,
        );

        tc.scope_future(span, async move {
            let fut = handler(&ctx, &job.payload);
            if let Some(timeout) = job.timeout {
                match tokio::time::timeout(timeout, fut).await {
                    Ok(res) => res,
                    Err(_) => Err(JobError::Timeout),
                }
            } else {
                fut.await
            }
        })
        .await
    }
}

/// RAII drop guard ensuring in-flight job counter is decremented even if the worker panics.
struct InFlightGuard(Arc<AtomicU32>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct InFlightHandleGuard {
    seq: u64,
    handles: Arc<std::sync::Mutex<std::collections::HashMap<u64, tokio::task::AbortHandle>>>,
}

impl Drop for InFlightHandleGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.handles.lock() {
            map.remove(&self.seq);
        }
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
    in_flight_handles:
        Arc<std::sync::Mutex<std::collections::HashMap<u64, tokio::task::AbortHandle>>>,
    worker_result: Option<bool>,
    heartbeat_result: Option<bool>,
    recovery_result: Option<bool>,
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

    /// Aborts all worker, auxiliary, and in-flight executing job tasks immediately.
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
        if let Ok(handles) = self.in_flight_handles.lock() {
            for handle in handles.values() {
                handle.abort();
            }
        }
    }

    /// Signals the worker to stop, waits for in-flight jobs to complete, and awaits all tasks.
    ///
    /// Does not consume `self`, allowing the caller to inspect task status or retry if shutdown times out.
    pub async fn try_shutdown(&mut self, timeout: Duration) -> bool {
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

        // 2. Await the worker execution loop (caching only an observed join outcome)
        let worker_ok = match self.worker_result {
            Some(ok) => ok,
            None => {
                let remaining = timeout.saturating_sub(start.elapsed());
                match await_supervised_task(&mut self.worker_task, remaining, "worker_execution")
                    .await
                {
                    Some(ok) => {
                        self.worker_result = Some(ok);
                        ok
                    }
                    None => false,
                }
            }
        };
        if !worker_ok {
            return false;
        }

        // 3. Await heartbeat task if present
        if let Some(hb) = self.heartbeat_task.as_mut() {
            let hb_ok = match self.heartbeat_result {
                Some(ok) => ok,
                None => {
                    let remaining = timeout.saturating_sub(start.elapsed());
                    match await_supervised_task(hb, remaining, "worker_heartbeat").await {
                        Some(ok) => {
                            self.heartbeat_result = Some(ok);
                            ok
                        }
                        None => false,
                    }
                }
            };
            if !hb_ok {
                return false;
            }
        }

        // 4. Await recovery task if present
        if let Some(rec) = self.recovery_task.as_mut() {
            let rec_ok = match self.recovery_result {
                Some(ok) => ok,
                None => {
                    let remaining = timeout.saturating_sub(start.elapsed());
                    match await_supervised_task(rec, remaining, "worker_recovery").await {
                        Some(ok) => {
                            self.recovery_result = Some(ok);
                            ok
                        }
                        None => false,
                    }
                }
            };
            if !rec_ok {
                return false;
            }
        }

        true
    }

    /// Signals the worker to stop, waits for in-flight jobs to complete, and awaits all tasks.
    ///
    /// If shutdown times out, all remaining background tasks are automatically aborted to prevent
    /// leaked or orphaned tasks. Returns `true` if cleanly shut down within `timeout`, or `false`
    /// if aborted after timeout expiration.
    pub async fn shutdown(mut self, timeout: Duration) -> bool {
        let clean = self.try_shutdown(timeout).await;
        if !clean {
            self.abort_all();
        }
        clean
    }
}

async fn await_supervised_task(
    task: &mut tokio::task::JoinHandle<()>,
    remaining: Duration,
    task_name: &'static str,
) -> Option<bool> {
    match tokio::time::timeout(remaining, task).await {
        Ok(Ok(())) => Some(true),
        Ok(Err(join_err)) => {
            if join_err.is_panic() {
                tracing::error!(
                    task = task_name,
                    "Worker background task panicked during execution"
                );
            } else if join_err.is_cancelled() {
                tracing::warn!(task = task_name, "Worker background task was cancelled");
            } else {
                tracing::error!(
                    task = task_name,
                    "Worker background task failed: {join_err}"
                );
            }
            Some(false)
        }
        Err(_) => {
            tracing::warn!(
                task = task_name,
                "Worker background task shutdown timed out"
            );
            None
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
    in_flight_handles:
        Arc<std::sync::Mutex<std::collections::HashMap<u64, tokio::task::AbortHandle>>>,
    next_job_seq: Arc<AtomicU64>,
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
            in_flight_handles: Arc::clone(&self.in_flight_handles),
            next_job_seq: Arc::clone(&self.next_job_seq),
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
            in_flight_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            next_job_seq: Arc::new(AtomicU64::new(0)),
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
            in_flight_handles: Arc::clone(&self.in_flight_handles),
            worker_result: None,
            heartbeat_result: None,
            recovery_result: None,
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

                    let job_span = tracing::info_span!(
                        "job.execute",
                        job.id = %job.id,
                        job.name = %job.name,
                        traceparent = job.trace_context.as_deref().unwrap_or("")
                    );

                    let job_seq = self.next_job_seq.fetch_add(1, Ordering::Relaxed);
                    let handles_map = Arc::clone(&self.in_flight_handles);
                    let handle_guard = InFlightHandleGuard {
                        seq: job_seq,
                        handles: Arc::clone(&handles_map),
                    };

                    let join_handle = tokio::spawn(
                        async move {
                            let _permit = permit;
                            let _guard = InFlightGuard(in_flight);
                            let _handle_guard = handle_guard;
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
                        }
                        .instrument(job_span),
                    );
                    if let Ok(mut map) = handles_map.lock() {
                        map.insert(job_seq, join_handle.abort_handle());
                    }
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
                    // Why: the handler did not execute, so deferring must not consume a retry attempt.
                    if let Err(err) = queue.defer(job, &token, Duration::from_secs(2)).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %err,
                            "Failed to defer in-progress idempotent job; lease may have expired or state mismatch"
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
                        "Failed to acquire idempotency lock; nacking job with backoff to fail closed"
                    );
                    if let Err(nack_err) = queue.nack(job, &token, Duration::from_secs(5)).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %nack_err,
                            "Failed to nack job after idempotency lock acquisition error"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    }
                    return;
                }
            }
        }

        // Derive heartbeat renewal timing directly from configured lease duration
        let lease_duration = job.lease_duration;
        let hb_interval = (lease_duration / 3)
            .max(Duration::from_millis(20))
            .min(Duration::from_secs(20));
        let renew_extension = lease_duration.max(Duration::from_secs(60));

        // Spawn background lease heartbeat while job is executing.
        // Separate cooperative shutdown cancellation (`job_cancelled`) from genuine lease/idempotency
        // ownership loss (`lease_lost`): a job that finishes cleanly during graceful shutdown while
        // its leases are still valid MUST be acked normally rather than dead-lettered.
        let job_cancelled = Arc::new(AtomicBool::new(stopping.load(Ordering::Relaxed)));
        let lease_lost = Arc::new(AtomicBool::new(false));
        let extend_idempotency = Arc::new(AtomicBool::new(true));
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let hb_token = token.clone();
        let hb_queue = Arc::clone(&queue);
        let hb_jid = job_id.clone();
        let hb_cancelled = Arc::clone(&job_cancelled);
        let hb_lease_lost = Arc::clone(&lease_lost);
        let hb_extend_idempotency = Arc::clone(&extend_idempotency);
        let hb_stopping = Arc::clone(&stopping);
        let hb_lost_leases = Arc::clone(&lost_leases);
        let hb_idemp_store = idempotency_store.clone();
        let hb_idemp_key = effective_key.clone();

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
                            hb_lease_lost.store(true, Ordering::SeqCst);
                            hb_lost_leases.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                        // 2. Extend distributed idempotency lock heartbeat while handler is running.
                        // CRITICAL DISTRIBUTED INVARIANT:
                        // If lock extension fails (e.g. Redis outage, network partition, or token mismatch where
                        // another worker claimed the lease), this worker has LOST idempotency ownership.
                        // Continuing to acknowledge the job upon completion would violate at-most-once/deduplication
                        // guarantees. Therefore, we immediately set `hb_lease_lost = true` and `hb_cancelled = true`,
                        // break the heartbeat loop, and route the completed job to the dead letter queue (DLQ) without acking.
                        if hb_extend_idempotency.load(Ordering::SeqCst)
                            && let (Some(store), Some(k)) = (&hb_idemp_store, &hb_idemp_key)
                            && let Err(e) = store.extend_lock_with_token(k, &hb_token, renew_extension).await
                        {
                            tracing::error!(
                                job_id = %hb_jid,
                                key = %k,
                                error = %e,
                                "Failed to extend idempotency lock heartbeat; lost idempotency ownership"
                            );
                            hb_cancelled.store(true, Ordering::SeqCst);
                            hb_lease_lost.store(true, Ordering::SeqCst);
                            hb_lost_leases.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
        });
        struct HeartbeatAbortGuard(tokio::task::AbortHandle);
        impl Drop for HeartbeatAbortGuard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _hb_abort_guard = HeartbeatAbortGuard(heartbeat_handle.abort_handle());

        let dispatch_result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.dispatch_with_cancellation(&job, Some(Arc::clone(&job_cancelled)))
        })) {
            Ok(fut) => match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
                Ok(res) => res,
                Err(_) => Err(JobError::Fatal("handler panicked".into())),
            },
            Err(_) => Err(JobError::Fatal("handler panicked".into())),
        };

        // Why: keep the queue lease heartbeat active until the post-dispatch state transition
        // (ack / nack / dead_letter) completes, so a slow idempotency release cannot let the
        // queue lease expire before `queue.nack` records the retry backoff. Stop extending the
        // idempotency lock once we begin releasing/completing it so release does not trigger
        // spurious `extend_lock_with_token` errors.
        match dispatch_result {
            Ok(()) => {
                if lease_lost.load(Ordering::SeqCst) {
                    tracing::warn!(
                        job_id = %job_id,
                        "Job execution completed after lease was lost; routing to dead letter queue"
                    );
                    if let Err(err) = queue
                        .dead_letter(job, &token, "lease lost during execution".into())
                        .await
                    {
                        tracing::error!(
                            job_id = %job_id,
                            error = %err,
                            "Failed to dead-letter job after lost lease"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    } else {
                        // Why: release the deduplication reservation only after `dead_letter`
                        // commits; if dead-lettering fails, preserving the reservation blocks
                        // another worker from re-executing a handler that already succeeded.
                        extend_idempotency.store(false, Ordering::SeqCst);
                        release_idempotency_token(
                            &idempotency_store,
                            &effective_key,
                            &job_id,
                            &token,
                            "on lease loss",
                        )
                        .await;
                    }
                } else {
                    if let Err(err) = queue.ack(&job_id, &token).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %err,
                            "Failed to ack job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    }
                    extend_idempotency.store(false, Ordering::SeqCst);
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
            }
            Err(JobError::Fatal(msg))
            | Err(JobError::UnknownJob(msg))
            | Err(JobError::Serialization(msg)) => {
                extend_idempotency.store(false, Ordering::SeqCst);
                release_idempotency_token(
                    &idempotency_store,
                    &effective_key,
                    &job_id,
                    &token,
                    "on fatal failure",
                )
                .await;
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
                // Why: lock ownership is retained only until the terminal transition
                // (success via ack or terminal failure dead-lettered). A retryable error is a
                // non-terminal transition, so release the distributed idempotency lock *before*
                // nacking so another worker can re-acquire the same scoped key and re-execute.
                // The lock is keyed by `effective_key` (the tenant-scoped idempotency key),
                // never by `job.id`.
                if let Some(backoff) = job.retry_policy.next_backoff(job.attempt) {
                    extend_idempotency.store(false, Ordering::SeqCst);
                    release_idempotency_token(
                        &idempotency_store,
                        &effective_key,
                        &job_id,
                        &token,
                        "before retryable nack; proceeding with nack",
                    )
                    .await;
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
                    extend_idempotency.store(false, Ordering::SeqCst);
                    release_idempotency_token(
                        &idempotency_store,
                        &effective_key,
                        &job_id,
                        &token,
                        "on dead letter",
                    )
                    .await;
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

        let _ = cancel_tx.send(());
        heartbeat_handle.abort();
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

/// Releases an idempotency token with a bounded wait so a stalled store cannot
/// indefinitely block worker state transitions.
async fn release_idempotency_token(
    store: &Option<Arc<dyn IdempotencyStore>>,
    key: &Option<String>,
    job_id: &str,
    token: &str,
    context: &str,
) {
    const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
    if let (Some(store), Some(key)) = (store, key) {
        match tokio::time::timeout(RELEASE_TIMEOUT, store.release_with_token(key, token)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!(
                    job_id = %job_id,
                    key = %key,
                    error = %err,
                    "Failed to release idempotency token {context}"
                );
            }
            Err(_) => {
                tracing::error!(
                    job_id = %job_id,
                    key = %key,
                    "Timed out releasing idempotency token {context}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};
    use crate::jobs::retry::RetryPolicy;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    use tokio::sync::{Mutex, mpsc};

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
        ack_count: Arc<AtomicUsize>,
        dlq_count: Arc<AtomicUsize>,
        fail_renew: bool,
    }

    impl JobQueue for BlockingTestQueue {
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
            _trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async move { Ok("test-id".to_string()) })
        }

        fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async move { Ok(None) })
        }

        fn dequeue_for(
            &self,
            worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
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
            self.ack_count.fetch_add(1, Ordering::SeqCst);
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
            self.dlq_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(()) })
        }

        fn renew_lease(
            &self,
            _job_id: &str,
            _lease_token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            if self.fail_renew {
                Box::pin(async move { Err(JobError::Fatal("lease renewal failed".into())) })
            } else {
                Box::pin(async move { Ok(()) })
            }
        }

        fn purge_completed(&self, _retention: Duration) -> BoxFuture<'_, Result<u64, JobError>> {
            Box::pin(async { Ok(5) })
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
            trace_context: None,
        };

        let queue = Arc::new(BlockingTestQueue {
            job: Mutex::new(Some(job)),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::clone(&nack_count),
            ack_count: Arc::new(AtomicUsize::new(0)),
            dlq_count: Arc::new(AtomicUsize::new(0)),
            fail_renew: false,
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
        let drain_task =
            tokio::spawn(async move { drain_worker.drain(Duration::from_secs(2)).await });

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
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "job must not execute after drain requested"
        );

        // 7. Verify the returned job was immediately nack'd back to queue
        assert_eq!(
            nack_count.load(Ordering::SeqCst),
            1,
            "unstarted job must be nack'd immediately"
        );
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
            in_flight_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            worker_result: None,
            heartbeat_result: None,
            recovery_result: None,
        };

        let success = handle.shutdown(Duration::from_secs(1)).await;
        assert!(
            !success,
            "WorkerHandle::shutdown must return false when a task panics"
        );
    }

    #[tokio::test]
    async fn test_worker_handle_timeout_aborts_tasks() {
        let is_stopping = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(tokio::sync::Notify::new());
        let in_flight_count = Arc::new(AtomicU32::new(0));
        let active_tasks = Arc::new(AtomicU32::new(1));

        // Long-running task that would sleep forever unless aborted
        let long_task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let abort_handle = long_task.abort_handle();

        let mut handle = WorkerHandle {
            worker_task: long_task,
            heartbeat_task: None,
            recovery_task: None,
            is_stopping,
            shutdown_notify,
            in_flight_count,
            active_tasks,
            in_flight_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            worker_result: None,
            heartbeat_result: None,
            recovery_result: None,
        };

        // 1. try_shutdown preserves handle on timeout without aborting immediately
        let clean = handle.try_shutdown(Duration::from_millis(50)).await;
        assert!(!clean, "try_shutdown must return false on timeout");
        assert!(
            !abort_handle.is_finished(),
            "task should still be running after try_shutdown"
        );

        // 2. consuming shutdown aborts remaining tasks on timeout
        let success = handle.shutdown(Duration::from_millis(50)).await;
        assert!(!success, "shutdown must return false on timeout");

        // Brief yield to allow the abort to process
        tokio::task::yield_now().await;
        // The worker task must have been aborted
        assert!(
            abort_handle.is_finished(),
            "task must be aborted on shutdown timeout"
        );
    }

    #[tokio::test]
    async fn test_worker_handle_try_shutdown_retry_after_heartbeat_timeout() {
        let worker_task = tokio::spawn(async {});
        let heartbeat_task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(60)).await;
        });

        let mut handle = WorkerHandle {
            worker_task,
            heartbeat_task: Some(heartbeat_task),
            recovery_task: None,
            is_stopping: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
            in_flight_count: Arc::new(AtomicU32::new(0)),
            active_tasks: Arc::new(AtomicU32::new(2)),
            in_flight_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            worker_result: None,
            heartbeat_result: None,
            recovery_result: None,
        };

        // First attempt: worker_task completes, heartbeat_task times out
        assert!(!handle.try_shutdown(Duration::from_millis(15)).await);
        // Second attempt: must NOT re-poll finished worker_task and must succeed once heartbeat finishes
        assert!(handle.try_shutdown(Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn test_purge_completed_queue_api() {
        let (enter_tx, _) = mpsc::channel(1);
        let (_, release_rx) = mpsc::channel(1);
        let queue = BlockingTestQueue {
            job: Mutex::new(None),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::new(AtomicUsize::new(0)),
            ack_count: Arc::new(AtomicUsize::new(0)),
            dlq_count: Arc::new(AtomicUsize::new(0)),
            fail_renew: false,
        };

        let purged = queue
            .purge_completed(Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(purged, 5);
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct SlowTestJob;
    impl Job for SlowTestJob {
        const NAME: &'static str = "SlowTestJob";
    }

    struct SlowTestHandler;
    impl JobHandler<SlowTestJob> for SlowTestHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: SlowTestJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn test_worker_heartbeat_loss_routes_to_dlq() {
        let (enter_tx, _) = mpsc::channel(1);
        let (_, release_rx) = mpsc::channel(1);
        let nack_count = Arc::new(AtomicUsize::new(0));
        let ack_count = Arc::new(AtomicUsize::new(0));
        let dlq_count = Arc::new(AtomicUsize::new(0));

        let payload_bytes = serde_json::to_vec(&SlowTestJob).unwrap();
        let job = QueuedJob {
            id: "job-dlq-test".to_string(),
            name: SlowTestJob::NAME.to_string(),
            payload: payload_bytes,
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: None,
            tenant_id: None,
            assigned_worker: Some("worker-1".into()),
            lease_token: Some("token-1".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(30),
            trace_context: None,
        };

        let queue = Arc::new(BlockingTestQueue {
            job: Mutex::new(None),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::clone(&nack_count),
            ack_count: Arc::clone(&ack_count),
            dlq_count: Arc::clone(&dlq_count),
            fail_renew: true,
        });

        let mut registry = JobRegistry::new();
        registry.register(SlowTestHandler);

        JobWorker::execute_job(
            Arc::clone(&queue),
            Arc::new(registry),
            job,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        assert_eq!(
            dlq_count.load(Ordering::SeqCst),
            1,
            "heartbeat lost job must route to DLQ"
        );
        assert_eq!(
            ack_count.load(Ordering::SeqCst),
            0,
            "heartbeat lost job must not be acked"
        );
    }

    #[derive(Debug)]
    struct FailingIdempotencyStore;

    impl IdempotencyStore for FailingIdempotencyStore {
        fn try_acquire_with_token(
            &self,
            _key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
            Box::pin(async move { Err(JobError::Fatal("redis outage".into())) })
        }

        fn complete_with_token(
            &self,
            _key: &str,
            _token: &str,
            _result: Option<Vec<u8>>,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn release_with_token(
            &self,
            _key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[tokio::test]
    async fn test_idempotency_store_acquisition_failure_fails_closed_and_nacks() {
        let (enter_tx, _enter_rx) = mpsc::channel(1);
        let (_release_tx, release_rx) = mpsc::channel(1);
        let nack_count = Arc::new(AtomicUsize::new(0));
        let ack_count = Arc::new(AtomicUsize::new(0));
        let dlq_count = Arc::new(AtomicUsize::new(0));

        let payload_bytes = serde_json::to_vec(&SlowTestJob).unwrap();
        let job = QueuedJob {
            id: "job-fail-closed".to_string(),
            name: SlowTestJob::NAME.to_string(),
            payload: payload_bytes,
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("idemp-fail-key".into()),
            tenant_id: None,
            assigned_worker: Some("worker-1".into()),
            lease_token: Some("token-1".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(30),
            trace_context: None,
        };

        let queue = Arc::new(BlockingTestQueue {
            job: Mutex::new(None),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::clone(&nack_count),
            ack_count: Arc::clone(&ack_count),
            dlq_count: Arc::clone(&dlq_count),
            fail_renew: false,
        });

        let mut registry = JobRegistry::new();
        registry.register(SlowTestHandler);

        let failing_store = Arc::new(FailingIdempotencyStore);

        JobWorker::execute_job(
            Arc::clone(&queue),
            Arc::new(registry),
            job,
            Some(failing_store),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        assert_eq!(
            nack_count.load(Ordering::SeqCst),
            1,
            "job must be nacked when idempotency acquisition fails"
        );
        assert_eq!(
            ack_count.load(Ordering::SeqCst),
            0,
            "handler must NOT execute or ack when idempotency acquisition fails"
        );
    }

    #[derive(Debug)]
    struct FailingRenewalIdempotencyStore;

    impl IdempotencyStore for FailingRenewalIdempotencyStore {
        fn try_acquire_with_token(
            &self,
            _key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
            Box::pin(async move { Ok(IdempotencyStatus::New) })
        }

        fn complete_with_token(
            &self,
            _key: &str,
            _token: &str,
            _result: Option<Vec<u8>>,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn release_with_token(
            &self,
            _key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn extend_lock_with_token(
            &self,
            _key: &str,
            _token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move {
                Err(JobError::Fatal(
                    "failed to extend idempotency key: token mismatch or lease expired".into(),
                ))
            })
        }
    }

    #[tokio::test]
    async fn test_worker_idempotency_renewal_failure_routes_to_dlq() {
        let (enter_tx, _enter_rx) = mpsc::channel(1);
        let (_release_tx, release_rx) = mpsc::channel(1);
        let nack_count = Arc::new(AtomicUsize::new(0));
        let ack_count = Arc::new(AtomicUsize::new(0));
        let dlq_count = Arc::new(AtomicUsize::new(0));

        let payload_bytes = serde_json::to_vec(&SlowTestJob).unwrap();
        let job = QueuedJob {
            id: "job-renew-fail".to_string(),
            name: SlowTestJob::NAME.to_string(),
            payload: payload_bytes,
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("idemp-renew-key".into()),
            tenant_id: None,
            assigned_worker: Some("worker-1".into()),
            lease_token: Some("token-1".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(30),
            trace_context: None,
        };

        let queue = Arc::new(BlockingTestQueue {
            job: Mutex::new(None),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::clone(&nack_count),
            ack_count: Arc::clone(&ack_count),
            dlq_count: Arc::clone(&dlq_count),
            fail_renew: false,
        });

        let mut registry = JobRegistry::new();
        registry.register(SlowTestHandler);

        let failing_store = Arc::new(FailingRenewalIdempotencyStore);

        JobWorker::execute_job(
            Arc::clone(&queue),
            Arc::new(registry),
            job,
            Some(failing_store),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        assert_eq!(
            dlq_count.load(Ordering::SeqCst),
            1,
            "job must be routed to DLQ when idempotency renewal heartbeat fails"
        );
        assert_eq!(
            ack_count.load(Ordering::SeqCst),
            0,
            "job must NOT be acked when idempotency renewal heartbeat fails"
        );
    }

    #[derive(Debug)]
    struct DefaultUnsupportedStore;

    impl IdempotencyStore for DefaultUnsupportedStore {
        fn try_acquire_with_token(
            &self,
            _key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
            Box::pin(async move { Ok(IdempotencyStatus::New) })
        }

        fn complete_with_token(
            &self,
            _key: &str,
            _token: &str,
            _result: Option<Vec<u8>>,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn release_with_token(
            &self,
            _key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }
    }

    #[tokio::test]
    async fn test_worker_default_unsupported_idempotency_renewal_routes_to_dlq() {
        let (enter_tx, _enter_rx) = mpsc::channel(1);
        let (_release_tx, release_rx) = mpsc::channel(1);
        let nack_count = Arc::new(AtomicUsize::new(0));
        let ack_count = Arc::new(AtomicUsize::new(0));
        let dlq_count = Arc::new(AtomicUsize::new(0));

        let payload_bytes = serde_json::to_vec(&SlowTestJob).unwrap();
        let job = QueuedJob {
            id: "job-unsupported-fail".to_string(),
            name: SlowTestJob::NAME.to_string(),
            payload: payload_bytes,
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("idemp-unsupported-key".into()),
            tenant_id: None,
            assigned_worker: Some("worker-1".into()),
            lease_token: Some("token-1".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(30),
            trace_context: None,
        };

        let queue = Arc::new(BlockingTestQueue {
            job: Mutex::new(None),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            nack_count: Arc::clone(&nack_count),
            ack_count: Arc::clone(&ack_count),
            dlq_count: Arc::clone(&dlq_count),
            fail_renew: false,
        });

        let mut registry = JobRegistry::new();
        registry.register(SlowTestHandler);

        let unsupported_store = Arc::new(DefaultUnsupportedStore);

        JobWorker::execute_job(
            Arc::clone(&queue),
            Arc::new(registry),
            job,
            Some(unsupported_store),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        assert_eq!(
            dlq_count.load(Ordering::SeqCst),
            1,
            "job must be routed to DLQ when store does not support heartbeat renewal"
        );
        assert_eq!(
            ack_count.load(Ordering::SeqCst),
            0,
            "job must NOT be acked when store does not support heartbeat renewal"
        );
    }

    /// A job whose handler always returns a retryable error, used to exercise the
    /// retryable-failure branch of [`JobWorker::execute_job`].
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct RetryFailJob;

    impl Job for RetryFailJob {
        const NAME: &'static str = "retry_fail_job";
    }

    struct RetryFailHandler;

    impl JobHandler<RetryFailJob> for RetryFailHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: RetryFailJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            Box::pin(async move { Err(JobError::Retryable("transient failure".into())) })
        }
    }

    /// Fake idempotency store that records every call into a shared sequence log,
    /// used to assert lock-release ordering relative to queue nack.
    #[derive(Debug, Clone, Default)]
    struct RecordingIdempotencyStore {
        seq_log: Arc<Mutex<Vec<String>>>,
    }

    impl IdempotencyStore for RecordingIdempotencyStore {
        fn try_acquire_with_token(
            &self,
            key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
            let key = key.to_string();
            let seq_log = Arc::clone(&self.seq_log);
            Box::pin(async move {
                seq_log.lock().await.push(format!("acquire:{key}"));
                Ok(IdempotencyStatus::New)
            })
        }

        fn complete_with_token(
            &self,
            _key: &str,
            _token: &str,
            _result: Option<Vec<u8>>,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn release_with_token(
            &self,
            key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let key = key.to_string();
            let seq_log = Arc::clone(&self.seq_log);
            Box::pin(async move {
                seq_log.lock().await.push(format!("release:{key}"));
                Ok(())
            })
        }

        fn extend_lock_with_token(
            &self,
            _key: &str,
            _token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }
    }

    /// Fake queue that records `nack` calls into a shared sequence log.
    #[derive(Debug, Clone, Default)]
    struct RecordingTestQueue {
        seq_log: Arc<Mutex<Vec<String>>>,
    }

    impl JobQueue for RecordingTestQueue {
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
            _trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async move { Ok("test-id".to_string()) })
        }

        fn dequeue_for(
            &self,
            _worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async move { Ok(None) })
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
            let id = job.id.clone();
            let seq_log = Arc::clone(&self.seq_log);
            Box::pin(async move {
                seq_log.lock().await.push(format!("nack:{id}"));
                Ok(())
            })
        }

        fn dead_letter(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn renew_lease(
            &self,
            _job_id: &str,
            _lease_token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async move { Ok(()) })
        }

        fn purge_completed(&self, _retention: Duration) -> BoxFuture<'_, Result<u64, JobError>> {
            Box::pin(async { Ok(0) })
        }

        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async move { Ok(0) })
        }

        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async move { Ok(0) })
        }
    }

    #[tokio::test]
    async fn test_worker_retryable_failure_releases_idempotency_lock_before_nack() {
        let seq_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingIdempotencyStore {
            seq_log: Arc::clone(&seq_log),
        });
        let queue = Arc::new(RecordingTestQueue {
            seq_log: Arc::clone(&seq_log),
        });

        let payload_bytes = serde_json::to_vec(&RetryFailJob).unwrap();
        let job = QueuedJob {
            id: "job-retry-lock".to_string(),
            name: RetryFailJob::NAME.to_string(),
            payload: payload_bytes,
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("idemp-key".into()),
            tenant_id: Some("tenant-a".into()),
            assigned_worker: Some("worker-1".into()),
            lease_token: Some("token-1".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_secs(60),
            trace_context: None,
        };

        let mut registry = JobRegistry::new();
        registry.register(RetryFailHandler);

        JobWorker::execute_job(
            Arc::clone(&queue),
            Arc::new(registry),
            job,
            Some(store),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        let log = seq_log.lock().await;
        let release_idx = log.iter().position(|e| e.starts_with("release:"));
        let nack_idx = log.iter().position(|e| e.starts_with("nack:"));

        assert!(
            release_idx.is_some(),
            "release_with_token must be called before nack on retryable failure; idempotency lock was leaked"
        );
        let release_idx = release_idx.expect("release present");
        let nack_idx = nack_idx.expect("nack present");
        assert!(
            release_idx < nack_idx,
            "release_with_token must precede queue.nack; log: {log:?}"
        );

        // The lock must be keyed by the scoped idempotency key (effective_key), not job.id.
        let release_entry = &log[release_idx];
        let expected_key = scoped_idempotency_key("tenant-a", "idemp-key");
        assert!(
            release_entry == &format!("release:{expected_key}"),
            "lock must be released by effective_key, not job.id; got {release_entry:?}"
        );
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct SlowOkJob;
    impl Job for SlowOkJob {
        const NAME: &'static str = "slow_ok_job";
    }
    struct SlowOkHandler(Duration);
    impl JobHandler<SlowOkJob> for SlowOkHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: SlowOkJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let delay = self.0;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(())
            })
        }
    }

    #[derive(Debug)]
    struct LeaseLossOrderQueue {
        seq_log: Arc<Mutex<Vec<String>>>,
        fail_dead_letter: bool,
    }

    impl JobQueue for LeaseLossOrderQueue {
        fn enqueue_delayed<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async { Ok("id".into()) })
        }
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
            _trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async { Ok("id".into()) })
        }
        fn dequeue_for(
            &self,
            _worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async { Ok(None) })
        }
        fn reclaim_abandoned_jobs(
            &self,
            _dead_workers: &[String],
        ) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn ack(&self, job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
            let log = Arc::clone(&self.seq_log);
            let id = job_id.to_string();
            Box::pin(async move {
                log.lock().await.push(format!("ack:{id}"));
                Ok(())
            })
        }
        fn nack(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn dead_letter(
            &self,
            job: QueuedJob,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let log = Arc::clone(&self.seq_log);
            let fail = self.fail_dead_letter;
            Box::pin(async move {
                log.lock().await.push(format!("dead_letter:{}", job.id));
                if fail {
                    Err(JobError::Fatal("dlq failed".into()))
                } else {
                    Ok(())
                }
            })
        }
        fn renew_lease(
            &self,
            _job_id: &str,
            _lease_token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Err(JobError::Fatal("lease lost".into())) })
        }
        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
    }

    #[tokio::test]
    async fn test_worker_lease_lost_releases_only_after_dead_letter_succeeds() {
        // Case A: dead_letter fails -> release_with_token must NOT be called
        let seq_fail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let store_fail = Arc::new(RecordingIdempotencyStore {
            seq_log: Arc::clone(&seq_fail),
        });
        let queue_fail = Arc::new(LeaseLossOrderQueue {
            seq_log: Arc::clone(&seq_fail),
            fail_dead_letter: true,
        });
        let job_a = QueuedJob {
            id: "job-dlq-fail".to_string(),
            name: SlowOkJob::NAME.to_string(),
            payload: serde_json::to_vec(&SlowOkJob).unwrap(),
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("k-fail".into()),
            tenant_id: None,
            assigned_worker: Some("w1".into()),
            lease_token: Some("tok-1".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(30),
            trace_context: None,
        };
        let mut reg_a = JobRegistry::new();
        reg_a.register(SlowOkHandler(Duration::from_millis(60)));
        let transition_errors = Arc::new(AtomicU64::new(0));
        JobWorker::execute_job(
            queue_fail,
            Arc::new(reg_a),
            job_a,
            Some(store_fail),
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&transition_errors),
            Arc::new(AtomicU64::new(0)),
        )
        .await;
        let log_a = seq_fail.lock().await;
        assert!(
            log_a.iter().any(|e| e == "dead_letter:job-dlq-fail"),
            "dead_letter must be attempted: {log_a:?}"
        );
        assert!(
            !log_a.iter().any(|e| e.starts_with("release:")),
            "release_with_token must NOT be called when dead_letter fails: {log_a:?}"
        );
        assert_eq!(transition_errors.load(Ordering::SeqCst), 1);

        // Case B: dead_letter succeeds -> release_with_token must be called AFTER dead_letter
        let seq_ok: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let store_ok = Arc::new(RecordingIdempotencyStore {
            seq_log: Arc::clone(&seq_ok),
        });
        let queue_ok = Arc::new(LeaseLossOrderQueue {
            seq_log: Arc::clone(&seq_ok),
            fail_dead_letter: false,
        });
        let job_b = QueuedJob {
            id: "job-dlq-ok".to_string(),
            name: SlowOkJob::NAME.to_string(),
            payload: serde_json::to_vec(&SlowOkJob).unwrap(),
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("k-ok".into()),
            tenant_id: None,
            assigned_worker: Some("w1".into()),
            lease_token: Some("tok-2".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(30),
            trace_context: None,
        };
        let mut reg_b = JobRegistry::new();
        reg_b.register(SlowOkHandler(Duration::from_millis(60)));
        JobWorker::execute_job(
            queue_ok,
            Arc::new(reg_b),
            job_b,
            Some(store_ok),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;
        let log_b = seq_ok.lock().await;
        let dlq_idx = log_b
            .iter()
            .position(|e| e == "dead_letter:job-dlq-ok")
            .expect("dead_letter recorded");
        let rel_idx = log_b
            .iter()
            .position(|e| e.starts_with("release:"))
            .expect("release recorded");
        assert!(
            dlq_idx < rel_idx,
            "dead_letter must precede release_with_token on lease loss: {log_b:?}"
        );
    }

    #[derive(Debug)]
    struct SlowReleaseIdempotencyStore {
        release_delay: Duration,
        seq_log: Arc<Mutex<Vec<String>>>,
    }

    impl IdempotencyStore for SlowReleaseIdempotencyStore {
        fn try_acquire_with_token(
            &self,
            _key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
            Box::pin(async { Ok(IdempotencyStatus::New) })
        }
        fn complete_with_token(
            &self,
            _key: &str,
            _token: &str,
            _result: Option<Vec<u8>>,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn release_with_token(
            &self,
            key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let delay = self.release_delay;
            let log = Arc::clone(&self.seq_log);
            let k = key.to_string();
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                log.lock().await.push(format!("release:{k}"));
                Ok(())
            })
        }
    }

    #[derive(Debug)]
    struct LeaseTrackingQueue {
        seq_log: Arc<Mutex<Vec<String>>>,
        renew_count: Arc<AtomicUsize>,
    }

    impl JobQueue for LeaseTrackingQueue {
        fn enqueue_delayed<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async { Ok("id".into()) })
        }
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
            _trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async { Ok("id".into()) })
        }
        fn dequeue_for(
            &self,
            _worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async { Ok(None) })
        }
        fn reclaim_abandoned_jobs(
            &self,
            _dead_workers: &[String],
        ) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn ack(&self, _job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn nack(
            &self,
            job: QueuedJob,
            _lease_token: &str,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let log = Arc::clone(&self.seq_log);
            Box::pin(async move {
                log.lock().await.push(format!("nack:{}", job.id));
                Ok(())
            })
        }
        fn dead_letter(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn renew_lease(
            &self,
            job_id: &str,
            _lease_token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let count = Arc::clone(&self.renew_count);
            let log = Arc::clone(&self.seq_log);
            let id = job_id.to_string();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                log.lock().await.push(format!("renew:{id}"));
                Ok(())
            })
        }
        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
    }

    #[tokio::test]
    async fn test_worker_keeps_heartbeat_active_until_nack_completes_on_slow_release() {
        let seq_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let renew_count = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(SlowReleaseIdempotencyStore {
            release_delay: Duration::from_millis(120),
            seq_log: Arc::clone(&seq_log),
        });
        let queue = Arc::new(LeaseTrackingQueue {
            seq_log: Arc::clone(&seq_log),
            renew_count: Arc::clone(&renew_count),
        });
        let job = QueuedJob {
            id: "job-slow-rel".to_string(),
            name: RetryFailJob::NAME.to_string(),
            payload: serde_json::to_vec(&RetryFailJob).unwrap(),
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("k-slow".into()),
            tenant_id: None,
            assigned_worker: Some("w1".into()),
            lease_token: Some("tok-slow".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_millis(45),
            trace_context: None,
        };
        let mut registry = JobRegistry::new();
        registry.register(RetryFailHandler);

        JobWorker::execute_job(
            queue,
            Arc::new(registry),
            job,
            Some(store),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        let log = seq_log.lock().await;
        assert!(
            renew_count.load(Ordering::SeqCst) >= 2,
            "heartbeat must keep renewing lease while release_with_token is slow: {log:?}"
        );
        let rel_idx = log
            .iter()
            .position(|e| e.starts_with("release:"))
            .expect("release");
        let nack_idx = log
            .iter()
            .position(|e| e == "nack:job-slow-rel")
            .expect("nack");
        assert!(rel_idx < nack_idx, "release must precede nack: {log:?}");
    }

    #[derive(Debug)]
    struct InProgressIdempotencyStore;
    impl IdempotencyStore for InProgressIdempotencyStore {
        fn try_acquire_with_token(
            &self,
            _key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<IdempotencyStatus, JobError>> {
            Box::pin(async { Ok(IdempotencyStatus::InProgress) })
        }
        fn complete_with_token(
            &self,
            _key: &str,
            _token: &str,
            _result: Option<Vec<u8>>,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn release_with_token(
            &self,
            _key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Debug)]
    struct DeferTrackingQueue {
        nack_calls: Arc<AtomicUsize>,
        defer_calls: Arc<AtomicUsize>,
        attempt_counter: Arc<AtomicU32>,
    }

    impl JobQueue for DeferTrackingQueue {
        fn enqueue_delayed<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async { Ok("id".into()) })
        }
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
            _trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async { Ok("id".into()) })
        }
        fn dequeue_for(
            &self,
            _worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async { Ok(None) })
        }
        fn reclaim_abandoned_jobs(
            &self,
            _dead_workers: &[String],
        ) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn ack(&self, _job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn nack(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            self.nack_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
        fn defer(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            self.defer_calls.fetch_add(1, Ordering::SeqCst);
            let mut current = self.attempt_counter.load(Ordering::SeqCst);
            while let Err(actual) = self.attempt_counter.compare_exchange(
                current,
                current.saturating_sub(1),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                current = actual;
            }
            Box::pin(async { Ok(()) })
        }
        fn dead_letter(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
    }

    #[tokio::test]
    async fn test_worker_in_progress_idempotency_defers_without_burning_retry_budget() {
        let nack_calls = Arc::new(AtomicUsize::new(0));
        let defer_calls = Arc::new(AtomicUsize::new(0));
        let attempt_counter = Arc::new(AtomicU32::new(2));
        let queue = Arc::new(DeferTrackingQueue {
            nack_calls: Arc::clone(&nack_calls),
            defer_calls: Arc::clone(&defer_calls),
            attempt_counter: Arc::clone(&attempt_counter),
        });
        let job = QueuedJob {
            id: "job-defer".to_string(),
            name: RetryFailJob::NAME.to_string(),
            payload: serde_json::to_vec(&RetryFailJob).unwrap(),
            attempt: 2,
            retry_policy: RetryPolicy::new().with_max_attempts(3),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: Some("k-inprog".into()),
            tenant_id: None,
            assigned_worker: Some("w1".into()),
            lease_token: Some("tok-defer".into()),
            available_at: Instant::now(),
            lease_duration: Duration::from_secs(60),
            trace_context: None,
        };
        let mut registry = JobRegistry::new();
        registry.register(RetryFailHandler);

        JobWorker::execute_job(
            queue,
            Arc::new(registry),
            job,
            Some(Arc::new(InProgressIdempotencyStore)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
        .await;

        assert_eq!(
            defer_calls.load(Ordering::SeqCst),
            1,
            "InProgress must call queue.defer"
        );
        assert_eq!(
            nack_calls.load(Ordering::SeqCst),
            0,
            "InProgress must not call queue.nack"
        );
        assert_eq!(
            attempt_counter.load(Ordering::SeqCst),
            1,
            "defer must restore attempt count so next claim does not burn retry budget"
        );
    }
}
