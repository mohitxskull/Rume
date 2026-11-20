use futures_util::FutureExt;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::sleep;
use tracing::Instrument;
use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::QueuedJob;
use crate::jobs::queue::JobQueue;
use crate::jobs::reliability::{
    HeartbeatRegistry, IdempotencyStatus, IdempotencyStore, scoped_idempotency_key,
};

use super::WorkerHandle;
use super::heartbeat::{release_idempotency_token, spawn_lease_heartbeat};
use super::registry::{InFlightGuard, JobRegistry};

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

/// RAII guard ensuring `active_tasks` is always decremented on task exit, panic, or abort.
struct ActiveTaskGuard(Arc<AtomicU32>);

impl ActiveTaskGuard {
    fn acquire(counter: &Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(counter))
    }
}

impl Drop for ActiveTaskGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Background worker managing queue polling, retries, and graceful shutdown drain.
pub struct JobWorker<Q: JobQueue> {
    worker_id: String,
    queue: Arc<Q>,
    registry: Arc<JobRegistry>,
    pub(super) is_stopping: Arc<AtomicBool>,
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
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
    abort_requested: Arc<AtomicBool>,
    abort_tx: Arc<tokio::sync::watch::Sender<bool>>,
    active_tasks: Arc<AtomicU32>,
    #[cfg(test)]
    pub(super) join_set_len: Arc<std::sync::atomic::AtomicUsize>,
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
            shutdown_tx: Arc::clone(&self.shutdown_tx),
            abort_requested: Arc::clone(&self.abort_requested),
            abort_tx: Arc::clone(&self.abort_tx),
            active_tasks: Arc::clone(&self.active_tasks),
            #[cfg(test)]
            join_set_len: Arc::clone(&self.join_set_len),
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
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (abort_tx, _) = tokio::sync::watch::channel(false);
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
            shutdown_tx: Arc::new(shutdown_tx),
            abort_requested: Arc::new(AtomicBool::new(false)),
            abort_tx: Arc::new(abort_tx),
            active_tasks: Arc::new(AtomicU32::new(0)),
            #[cfg(test)]
            join_set_len: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
        let worker_task_guard = ActiveTaskGuard::acquire(&self.active_tasks);
        let worker = self.clone();
        let worker_task = tokio::spawn(async move {
            let _guard = worker_task_guard;
            worker.run_loop().await;
        });

        let mut heartbeat_task = None;
        let mut recovery_task = None;

        if let Some(heartbeat) = self.heartbeat_registry.clone() {
            let worker_id = self.worker_id.clone();
            let mut shutdown_rx = self.shutdown_tx.subscribe();
            let hb_guard = ActiveTaskGuard::acquire(&self.active_tasks);
            let ttl = self.heartbeat_ttl;
            let interval = self.heartbeat_interval;
            let hb_loop = Arc::clone(&heartbeat);

            heartbeat_task = Some(tokio::spawn(async move {
                let _guard = hb_guard;
                while !*shutdown_rx.borrow() {
                    if let Err(e) = hb_loop.record_heartbeat(&worker_id, ttl).await {
                        tracing::warn!(worker_id = %worker_id, error = %e, "Failed to record worker heartbeat");
                    }
                    tokio::select! {
                        biased;
                        _ = shutdown_rx.wait_for(|&stopped| stopped) => { break; }
                        _ = sleep(interval) => {},
                    }
                }
            }));

            if let Some(recovery_interval) = self.abandoned_recovery_interval {
                let queue = Arc::clone(&self.queue);
                let mut shutdown_rx = self.shutdown_tx.subscribe();
                let rec_guard = ActiveTaskGuard::acquire(&self.active_tasks);
                let hb_reap = Arc::clone(&heartbeat);

                recovery_task = Some(tokio::spawn(async move {
                    let _guard = rec_guard;
                    while !*shutdown_rx.borrow() {
                        tokio::select! {
                            biased;
                            _ = shutdown_rx.wait_for(|&stopped| stopped) => { break; }
                            _ = sleep(recovery_interval) => {},
                        }
                        if *shutdown_rx.borrow() {
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
                }));
            }
        }

        WorkerHandle {
            worker_task,
            heartbeat_task,
            recovery_task,
            is_stopping: Arc::clone(&self.is_stopping),
            shutdown_tx: Arc::clone(&self.shutdown_tx),
            abort_requested: Arc::clone(&self.abort_requested),
            abort_tx: Arc::clone(&self.abort_tx),
            in_flight_count: Arc::clone(&self.in_flight_count),
            active_tasks: Arc::clone(&self.active_tasks),
            worker_result: None,
            heartbeat_result: None,
            recovery_result: None,
        }
    }

    fn record_join_set_len(&self, _len: usize) {
        #[cfg(test)]
        self.join_set_len.store(_len, Ordering::SeqCst);
    }

    fn log_in_flight_outcome(outcome: Result<(), tokio::task::JoinError>) {
        if let Err(join_err) = outcome {
            if join_err.is_panic() {
                tracing::error!("In-flight job task panicked unexpectedly: {join_err}");
            } else if join_err.is_cancelled() {
                tracing::debug!("In-flight job task was cancelled");
            } else {
                tracing::error!("In-flight job task failed: {join_err}");
            }
        }
    }

    async fn sleep_while_reaping(
        &self,
        duration: Duration,
        in_flight_tasks: &mut tokio::task::JoinSet<()>,
    ) -> bool {
        let timer = sleep(duration);
        tokio::pin!(timer);
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        loop {
            if *shutdown_rx.borrow() || self.is_stopping.load(Ordering::SeqCst) {
                return true;
            }
            tokio::select! {
                biased;
                _ = shutdown_rx.wait_for(|&stopped| stopped) => return true,
                _ = &mut timer => return false,
                res = in_flight_tasks.join_next(), if !in_flight_tasks.is_empty() => {
                    if let Some(outcome) = res {
                        Self::log_in_flight_outcome(outcome);
                    }
                    self.record_join_set_len(in_flight_tasks.len());
                }
            }
        }
    }

    /// Internal worker execution loop.
    async fn run_loop(&self) {
        // Why: own all spawned job tasks in a local `tokio::task::JoinSet` instead of a shared
        // `Mutex<HashMap<u64, AbortHandle>>` paired with a per-task map-removal guard; `JoinSet`
        // eliminates the spawn-before-insert race (where a fast-completing task could drop its
        // removal guard before `map.insert` and leak an `AbortHandle`), removes per-job mutex
        // contention, reaps completed `JoinHandle`s in steady state, and aborts all remaining
        // child tasks on drop or `shutdown()`. Revisit only if callers outside `run_loop` need
        // per-job abort handles.
        let mut in_flight_tasks = tokio::task::JoinSet::<()>::new();
        let mut consecutive_errors: u32 = 0;
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            while let Some(outcome) = in_flight_tasks.try_join_next() {
                Self::log_in_flight_outcome(outcome);
            }
            self.record_join_set_len(in_flight_tasks.len());

            if *shutdown_rx.borrow() || self.is_stopping.load(Ordering::SeqCst) {
                break;
            }

            let permit = tokio::select! {
                biased;
                _ = shutdown_rx.wait_for(|&stopped| stopped) => break,
                res = self.semaphore.clone().acquire_owned() => match res {
                    Ok(permit) => permit,
                    Err(_) => break,
                },
                res = in_flight_tasks.join_next(), if !in_flight_tasks.is_empty() => {
                    if let Some(outcome) = res {
                        Self::log_in_flight_outcome(outcome);
                    }
                    self.record_join_set_len(in_flight_tasks.len());
                    continue;
                }
            };

            if *shutdown_rx.borrow() || self.is_stopping.load(Ordering::SeqCst) {
                drop(permit);
                break;
            }

            match self.queue.dequeue_for(&self.worker_id).await {
                Ok(Some(job)) => {
                    consecutive_errors = 0;
                    if *shutdown_rx.borrow() || self.is_stopping.load(Ordering::SeqCst) {
                        // Crucial race check: draining started while dequeue was in-flight!
                        // Guarantee that no new job starts after draining begins, and use
                        // `defer` rather than `nack` so the unexecuted job does not burn a
                        // retry attempt.
                        let job_id = job.id.clone();
                        let token = job.lease_token.clone().unwrap_or_default();
                        if let Err(err) = self.queue.defer(job, &token, Duration::ZERO).await {
                            tracing::error!(
                                job_id = %job_id,
                                error = %err,
                                "Failed to defer dequeued job during drain"
                            );
                            self.transition_errors.fetch_add(1, Ordering::SeqCst);
                        }
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

                    in_flight_tasks.spawn(
                        async move {
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
                        }
                        .instrument(job_span),
                    );
                    while let Some(outcome) = in_flight_tasks.try_join_next() {
                        Self::log_in_flight_outcome(outcome);
                    }
                    self.record_join_set_len(in_flight_tasks.len());
                }
                Ok(None) => {
                    consecutive_errors = 0;
                    drop(permit);
                    if self
                        .sleep_while_reaping(Duration::from_millis(25), &mut in_flight_tasks)
                        .await
                    {
                        break;
                    }
                }
                Err(_) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    drop(permit);
                    let backoff_ms = (50u64 * 2u64.pow(consecutive_errors.min(6))).min(5000);
                    if self
                        .sleep_while_reaping(
                            Duration::from_millis(backoff_ms),
                            &mut in_flight_tasks,
                        )
                        .await
                    {
                        break;
                    }
                }
            }
        }

        // Drain all active job tasks on graceful shutdown, or abort immediately if shutdown
        // timed out and `WorkerHandle::shutdown` signaled `abort_tx`.
        let mut abort_rx = self.abort_tx.subscribe();
        while !in_flight_tasks.is_empty() {
            if *abort_rx.borrow() || self.abort_requested.load(Ordering::SeqCst) {
                in_flight_tasks.shutdown().await;
                break;
            }
            let should_abort = tokio::select! {
                biased;
                _ = async { let _ = abort_rx.wait_for(|&aborted| aborted).await; } => true,
                res = in_flight_tasks.join_next() => {
                    if let Some(outcome) = res {
                        Self::log_in_flight_outcome(outcome);
                    }
                    false
                }
            };
            if should_abort {
                in_flight_tasks.shutdown().await;
                break;
            }
            self.record_join_set_len(in_flight_tasks.len());
        }
        self.record_join_set_len(in_flight_tasks.len());
    }

    /// Executes a single dequeued job, handling acknowledgments, retries, and dead-lettering.
    pub(super) async fn execute_job(
        queue: Arc<Q>,
        registry: Arc<JobRegistry>,
        job: QueuedJob,
        idempotency_store: Option<Arc<dyn IdempotencyStore>>,
        stopping: Arc<AtomicBool>,
        transition_errors: Arc<AtomicU64>,
        lost_leases: Arc<AtomicU64>,
    ) {
        let job_id = job.id.clone();
        let effective_key = match (&job.tenant_id, &job.idempotency_key) {
            (Some(tenant), Some(key)) => Some(scoped_idempotency_key(tenant, key)),
            (None, Some(key)) => Some(key.clone()),
            _ => None,
        };

        // Why: when an idempotency store is attached and the job carries an
        // idempotency key, a missing/empty `lease_token` cannot safely fence
        // Redis/store ownership. Fail closed instead of falling back to `""`.
        if idempotency_store.is_some()
            && effective_key.is_some()
            && job.lease_token.as_deref().is_none_or(str::is_empty)
        {
            tracing::error!(
                job_id = %job_id,
                "Refusing to execute idempotent job without a fencing lease_token"
            );
            transition_errors.fetch_add(1, Ordering::SeqCst);
            return;
        }

        let token = job.lease_token.clone().unwrap_or_default();

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

        // Spawn the per-job lease heartbeat while the job is executing.
        // Separate cooperative shutdown cancellation (`job_cancelled`) from genuine lease/idempotency
        // ownership loss (`lease_lost`): a job that finishes cleanly during graceful shutdown while
        // its leases are still valid MUST be acked normally rather than dead-lettered.
        let lease_duration = job.lease_duration;
        let hb = spawn_lease_heartbeat(
            Arc::clone(&queue),
            &job_id,
            &token,
            stopping,
            Arc::clone(&lost_leases),
            idempotency_store.clone(),
            effective_key.clone(),
            lease_duration,
        );

        let dispatch_result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.dispatch_with_cancellation(&job, Some(Arc::clone(&hb.job_cancelled)))
        })) {
            Ok(fut) => match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
                Ok(res) => res,
                Err(_) => Err(JobError::Fatal("handler panicked".into())),
            },
            Err(_) => Err(JobError::Fatal("handler panicked".into())),
        };

        // Why: wait for any already-in-flight heartbeat ownership check (`renew_lease` /
        // `extend_lock_with_token`) to settle before reading `lease_lost` and choosing
        // between ACK and dead-letter, while keeping subsequent heartbeat ticks eligible
        // during `ack`/`nack`/`dead_letter` until the state transition actually commits.
        drop(hb.tick_lock.lock().await);

        match dispatch_result {
            Ok(()) => {
                if hb.lease_lost.load(Ordering::SeqCst) {
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
                        hb.queue_committed.store(true, Ordering::SeqCst);
                        // Why: release the deduplication reservation only after `dead_letter`
                        // commits; if dead-lettering fails, preserving the reservation blocks
                        // another worker from re-executing a handler that already succeeded.
                        hb.extend_idempotency.store(false, Ordering::SeqCst);
                        release_idempotency_token(
                            &idempotency_store,
                            &effective_key,
                            &job_id,
                            &token,
                            "on lease loss",
                        )
                        .await;
                        hb.idemp_committed.store(true, Ordering::SeqCst);
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
                    } else {
                        hb.queue_committed.store(true, Ordering::SeqCst);
                    }
                    hb.extend_idempotency.store(false, Ordering::SeqCst);
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
                    hb.idemp_committed.store(true, Ordering::SeqCst);
                }
            }
            Err(JobError::Fatal(msg))
            | Err(JobError::UnknownJob(msg))
            | Err(JobError::Serialization(msg)) => {
                hb.extend_idempotency.store(false, Ordering::SeqCst);
                release_idempotency_token(
                    &idempotency_store,
                    &effective_key,
                    &job_id,
                    &token,
                    "on fatal failure",
                )
                .await;
                hb.idemp_committed.store(true, Ordering::SeqCst);
                if let Err(err) = queue.dead_letter(job, &token, msg).await {
                    tracing::error!(
                        job_id = %job_id,
                        error = %err,
                        "Failed to dead-letter job; lease may have expired or state mismatch"
                    );
                    transition_errors.fetch_add(1, Ordering::SeqCst);
                    lost_leases.fetch_add(1, Ordering::SeqCst);
                } else {
                    hb.queue_committed.store(true, Ordering::SeqCst);
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
                    hb.extend_idempotency.store(false, Ordering::SeqCst);
                    release_idempotency_token(
                        &idempotency_store,
                        &effective_key,
                        &job_id,
                        &token,
                        "before retryable nack; proceeding with nack",
                    )
                    .await;
                    hb.idemp_committed.store(true, Ordering::SeqCst);
                    if let Err(nack_err) = queue.nack(job, &token, backoff).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %nack_err,
                            "Failed to nack job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    } else {
                        hb.queue_committed.store(true, Ordering::SeqCst);
                    }
                } else {
                    hb.extend_idempotency.store(false, Ordering::SeqCst);
                    release_idempotency_token(
                        &idempotency_store,
                        &effective_key,
                        &job_id,
                        &token,
                        "on dead letter",
                    )
                    .await;
                    hb.idemp_committed.store(true, Ordering::SeqCst);
                    let reason = format!("max retries exceeded ({}) - error: {err}", job.attempt);
                    if let Err(dl_err) = queue.dead_letter(job, &token, reason).await {
                        tracing::error!(
                            job_id = %job_id,
                            error = %dl_err,
                            "Failed to dead-letter job; lease may have expired or state mismatch"
                        );
                        transition_errors.fetch_add(1, Ordering::SeqCst);
                        lost_leases.fetch_add(1, Ordering::SeqCst);
                    } else {
                        hb.queue_committed.store(true, Ordering::SeqCst);
                    }
                }
            }
        }

        hb.finish().await;
    }

    /// Gracefully requests worker shutdown and waits for in-flight jobs and background polling tasks to complete within `timeout`.
    pub async fn drain(&self, timeout: Duration) -> bool {
        self.is_stopping.store(true, Ordering::SeqCst);
        self.shutdown_tx.send_replace(true);
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
