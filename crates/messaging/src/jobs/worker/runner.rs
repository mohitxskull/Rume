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
use super::heartbeat::{LeaseHeartbeatParams, release_idempotency_token, spawn_lease_heartbeat};
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
    admission_lock: Arc<std::sync::Mutex<()>>,
    drain_dirty: Arc<AtomicBool>,
    in_flight_count: Arc<AtomicU32>,
    in_flight_zero: Arc<tokio::sync::Notify>,
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
            admission_lock: Arc::clone(&self.admission_lock),
            drain_dirty: Arc::clone(&self.drain_dirty),
            in_flight_count: Arc::clone(&self.in_flight_count),
            in_flight_zero: Arc::clone(&self.in_flight_zero),
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
        let worker_id = format!("worker-{}", Uuid::new_v4());
        Self {
            worker_id,
            queue,
            registry,
            is_stopping: Arc::new(AtomicBool::new(false)),
            admission_lock: Arc::new(std::sync::Mutex::new(())),
            drain_dirty: Arc::new(AtomicBool::new(false)),
            in_flight_count: Arc::new(AtomicU32::new(0)),
            in_flight_zero: Arc::new(tokio::sync::Notify::new()),
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
        // Why: snapshot `worker_id` per `start()` invocation so cloning a running
        // `JobWorker` and calling `.with_worker_id(...)` on the clone never mutates
        // the live polling or heartbeat tasks of the already-started worker.
        let run_worker_id = self.worker_id.clone();
        let mut heartbeat_task = None;
        let mut recovery_task = None;

        if let Some(heartbeat) = self.heartbeat_registry.clone() {
            let hb_worker_id = run_worker_id.clone();
            let mut shutdown_rx = self.shutdown_tx.subscribe();
            let hb_guard = ActiveTaskGuard::acquire(&self.active_tasks);
            let ttl = self.heartbeat_ttl;
            let interval = self.heartbeat_interval;
            let hb_loop = Arc::clone(&heartbeat);

            heartbeat_task = Some(tokio::spawn(async move {
                let _guard = hb_guard;
                while !*shutdown_rx.borrow() {
                    if let Err(e) = hb_loop.record_heartbeat(&hb_worker_id, ttl).await {
                        tracing::warn!(
                            worker_id = %hb_worker_id,
                            error = %e,
                            "Failed to record worker heartbeat"
                        );
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
                    // Why: `reap_dead_workers` removes expired worker IDs from Redis before
                    // returning them. Only reap a new batch when `pending_dead` is empty so a
                    // Postgres outage cannot grow `pending_dead` without bound; drop any worker
                    // that has since resumed in Redis (`is_alive == true`); and clear
                    // `pending_dead` only after `has_in_flight_jobs_for_workers` confirms no
                    // locked/remaining claims exist.
                    let mut pending_dead: Vec<String> = Vec::new();
                    while !*shutdown_rx.borrow() {
                        tokio::select! {
                            biased;
                            _ = shutdown_rx.wait_for(|&stopped| stopped) => { break; }
                            _ = sleep(recovery_interval) => {},
                        }
                        if *shutdown_rx.borrow() {
                            break;
                        }
                        if pending_dead.is_empty() {
                            match hb_reap.reap_dead_workers().await {
                                Ok(dead) => {
                                    pending_dead = dead;
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to reap dead workers during recovery");
                                }
                            }
                        }
                        if !pending_dead.is_empty() {
                            let mut still_dead = Vec::with_capacity(pending_dead.len());
                            for wid in pending_dead.drain(..) {
                                match hb_reap.is_alive(&wid).await {
                                    Ok(true) => {
                                        tracing::info!(
                                            worker_id = %wid,
                                            "Skipping abandoned-job reclaim for worker that resumed heartbeat"
                                        );
                                    }
                                    Ok(false) | Err(_) => still_dead.push(wid),
                                }
                            }
                            pending_dead = still_dead;
                        }
                        if !pending_dead.is_empty() {
                            match queue.reclaim_abandoned_jobs(&pending_dead).await {
                                Ok(_) => {
                                    match queue.has_in_flight_jobs_for_workers(&pending_dead).await
                                    {
                                        Ok(false) => pending_dead.clear(),
                                        Ok(true) => {
                                            tracing::debug!(
                                                dead_workers = ?pending_dead,
                                                "Locked or remaining in-flight claims exist for dead workers; retaining batch for next recovery tick"
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                dead_workers = ?pending_dead,
                                                "Failed to verify remaining in-flight claims; retaining batch for retry"
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(
                                        error = %e,
                                        dead_workers = ?pending_dead,
                                        "Failed to reclaim abandoned jobs; retaining batch for retry"
                                    );
                                }
                            }
                        }
                    }
                }));
            }
        }

        let worker_task_guard = ActiveTaskGuard::acquire(&self.active_tasks);
        let worker = self.clone();
        let worker_task = tokio::spawn(async move {
            let _guard = worker_task_guard;
            worker.run_loop(run_worker_id).await;
        });

        WorkerHandle {
            worker_task,
            heartbeat_task,
            recovery_task,
            is_stopping: Arc::clone(&self.is_stopping),
            admission_lock: Arc::clone(&self.admission_lock),
            drain_dirty: Arc::clone(&self.drain_dirty),
            shutdown_tx: Arc::clone(&self.shutdown_tx),
            abort_requested: Arc::clone(&self.abort_requested),
            abort_tx: Arc::clone(&self.abort_tx),
            in_flight_count: Arc::clone(&self.in_flight_count),
            in_flight_zero: Arc::clone(&self.in_flight_zero),
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
    async fn run_loop(&self, run_worker_id: String) {
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

            match self.queue.dequeue_for(&run_worker_id).await {
                Ok(Some(job)) => {
                    consecutive_errors = 0;

                    // Why: serialize job admission with the `is_stopping` transition under
                    // `admission_lock` so no new job can start after draining begins, and
                    // construct `InFlightGuard` before `in_flight_tasks.spawn` so cancelling
                    // an unpolled child task still decrements `in_flight_count`.
                    let admitted_guard = {
                        let _guard = self
                            .admission_lock
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if *shutdown_rx.borrow() || self.is_stopping.load(Ordering::SeqCst) {
                            None
                        } else {
                            self.in_flight_count.fetch_add(1, Ordering::SeqCst);
                            Some(InFlightGuard {
                                count: Arc::clone(&self.in_flight_count),
                                zero_notify: Arc::clone(&self.in_flight_zero),
                            })
                        }
                    };

                    let Some(in_flight_guard) = admitted_guard else {
                        // Crucial race check: draining started while dequeue was in-flight!
                        // Defer the unexecuted job without burning a retry attempt, retrying
                        // transient defer errors briefly and marking the drain dirty if
                        // deferral still fails.
                        let job_id = job.id.clone();
                        let token = job.lease_token.clone().unwrap_or_default();
                        let mut deferred = false;
                        for attempt in 0..3u32 {
                            match self.queue.defer(job.clone(), &token, Duration::ZERO).await {
                                Ok(()) => {
                                    deferred = true;
                                    break;
                                }
                                Err(err) => {
                                    tracing::error!(
                                        job_id = %job_id,
                                        attempt = attempt + 1,
                                        error = %err,
                                        "Failed to defer dequeued job during drain"
                                    );
                                    if self.abort_requested.load(Ordering::SeqCst) || attempt == 2 {
                                        break;
                                    }
                                    sleep(Duration::from_millis(25 * u64::from(attempt + 1))).await;
                                }
                            }
                        }
                        if !deferred {
                            self.transition_errors.fetch_add(1, Ordering::SeqCst);
                            self.drain_dirty.store(true, Ordering::SeqCst);
                        }
                        drop(permit);
                        break;
                    };

                    let queue = Arc::clone(&self.queue);
                    let registry = Arc::clone(&self.registry);
                    let idempotency = self.idempotency_store.clone();
                    let stopping = Arc::clone(&self.is_stopping);
                    let job_shutdown_rx = self.shutdown_tx.subscribe();
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
                            let _guard = in_flight_guard;
                            Self::execute_job_with_shutdown(
                                queue,
                                registry,
                                job,
                                idempotency,
                                stopping,
                                Some(job_shutdown_rx),
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
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn execute_job(
        queue: Arc<Q>,
        registry: Arc<JobRegistry>,
        job: QueuedJob,
        idempotency_store: Option<Arc<dyn IdempotencyStore>>,
        stopping: Arc<AtomicBool>,
        transition_errors: Arc<AtomicU64>,
        lost_leases: Arc<AtomicU64>,
    ) {
        Self::execute_job_with_shutdown(
            queue,
            registry,
            job,
            idempotency_store,
            stopping,
            None,
            transition_errors,
            lost_leases,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_job_with_shutdown(
        queue: Arc<Q>,
        registry: Arc<JobRegistry>,
        job: QueuedJob,
        idempotency_store: Option<Arc<dyn IdempotencyStore>>,
        stopping: Arc<AtomicBool>,
        shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
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
        let hb = spawn_lease_heartbeat(LeaseHeartbeatParams {
            queue: Arc::clone(&queue),
            job_id: job_id.clone(),
            token: token.clone(),
            stopping,
            shutdown_rx,
            lost_leases: Arc::clone(&lost_leases),
            idempotency_store: idempotency_store.clone(),
            effective_key: effective_key.clone(),
            lease_duration,
        });

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
        // during finalization until the state transition actually commits.
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
                        if release_idempotency_token(
                            &idempotency_store,
                            &effective_key,
                            &job_id,
                            &token,
                            "on lease loss",
                        )
                        .await
                        {
                            hb.idemp_committed.store(true, Ordering::SeqCst);
                        }
                    }
                } else if let (Some(store), Some(key)) = (&idempotency_store, &effective_key) {
                    // Why: on keyed jobs, finalize token-validated idempotency completion BEFORE
                    // `queue.ack` while keeping the queue lease renewed. If Redis rejects the
                    // fencing token during finalization, PostgreSQL must not record a false ACK;
                    // conversely, if `complete_with_token` succeeds and `queue.ack` later fails,
                    // the completed idempotency record stays intact so the next retry observes
                    // `IdempotencyStatus::Completed` and ACKs without re-running the handler.
                    // Bound the wait so a hung Redis call never permanently holds the semaphore
                    // permit or in-flight slot.
                    const COMPLETE_TIMEOUT: Duration = Duration::from_secs(5);
                    match tokio::time::timeout(
                        COMPLETE_TIMEOUT,
                        store.complete_with_token(key, &token, None),
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            hb.extend_idempotency.store(false, Ordering::SeqCst);
                            hb.idemp_committed.store(true, Ordering::SeqCst);
                            if let Err(err) = queue.ack(&job_id, &token).await {
                                tracing::error!(
                                    job_id = %job_id,
                                    error = %err,
                                    "Failed to ack job after idempotency completion; completed record preserved for retry"
                                );
                                transition_errors.fetch_add(1, Ordering::SeqCst);
                                lost_leases.fetch_add(1, Ordering::SeqCst);
                            } else {
                                hb.queue_committed.store(true, Ordering::SeqCst);
                            }
                        }
                        Ok(Err(JobError::IdempotencyLockLost(err))) => {
                            // Why: explicit fencing-token rejection confirms Redis did NOT commit
                            // completion under our ownership, so refuse ACK and dead-letter.
                            tracing::error!(
                                job_id = %job_id,
                                key = %key,
                                error = %err,
                                "Idempotency completion rejected due to lost token ownership; refusing to ACK and routing to DLQ"
                            );
                            transition_errors.fetch_add(1, Ordering::SeqCst);
                            lost_leases.fetch_add(1, Ordering::SeqCst);
                            if let Err(dl_err) = queue
                                .dead_letter(
                                    job,
                                    &token,
                                    format!("idempotency completion rejected: {err}"),
                                )
                                .await
                            {
                                tracing::error!(
                                    job_id = %job_id,
                                    error = %dl_err,
                                    "Failed to dead-letter job after idempotency token rejection"
                                );
                                transition_errors.fetch_add(1, Ordering::SeqCst);
                            } else {
                                hb.queue_committed.store(true, Ordering::SeqCst);
                            }
                        }
                        Ok(Err(err)) => {
                            // Why: a transport/I/O error leaves the commit outcome unknown (Redis
                            // may have committed `COMPLETED` before the response was lost).
                            // Reconcile against the store before deciding: if `COMPLETED` is
                            // observed, proceed to ACK; otherwise leave the job unacked (never
                            // dead-lettering a potentially succeeded job) so lease expiry drives
                            // safe recovery.
                            tracing::warn!(
                                job_id = %job_id,
                                key = %key,
                                error = %err,
                                "Transport error recording idempotency completion; reconciling store state"
                            );
                            match tokio::time::timeout(
                                COMPLETE_TIMEOUT,
                                store.try_acquire_with_token(key, &token, Duration::from_secs(1)),
                            )
                            .await
                            {
                                Ok(Ok(IdempotencyStatus::Completed(_))) => {
                                    hb.extend_idempotency.store(false, Ordering::SeqCst);
                                    hb.idemp_committed.store(true, Ordering::SeqCst);
                                    if let Err(ack_err) = queue.ack(&job_id, &token).await {
                                        tracing::error!(
                                            job_id = %job_id,
                                            error = %ack_err,
                                            "Failed to ack job after reconciling idempotency completion"
                                        );
                                        transition_errors.fetch_add(1, Ordering::SeqCst);
                                        lost_leases.fetch_add(1, Ordering::SeqCst);
                                    } else {
                                        hb.queue_committed.store(true, Ordering::SeqCst);
                                    }
                                }
                                _ => {
                                    tracing::error!(
                                        job_id = %job_id,
                                        key = %key,
                                        "Idempotency completion outcome remains unknown after reconciliation; skipping ACK to preserve recovery path"
                                    );
                                    transition_errors.fetch_add(1, Ordering::SeqCst);
                                    lost_leases.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                        }
                        Err(_) => {
                            // Why: a timeout leaves the Redis commit outcome unknown, so do NOT
                            // mark `idemp_committed`, do NOT ACK, and do NOT release the token;
                            // releasing the worker slot lets lease expiry drive safe recovery
                            // (a subsequent worker will see `Completed` if Redis committed).
                            tracing::error!(
                                job_id = %job_id,
                                key = %key,
                                "Timed out recording idempotency completion; skipping ACK to preserve recovery path"
                            );
                            transition_errors.fetch_add(1, Ordering::SeqCst);
                            lost_leases.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                } else if let Err(err) = queue.ack(&job_id, &token).await {
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
            }
            Err(JobError::Fatal(msg))
            | Err(JobError::UnknownJob(msg))
            | Err(JobError::Serialization(msg)) => {
                hb.extend_idempotency.store(false, Ordering::SeqCst);
                if release_idempotency_token(
                    &idempotency_store,
                    &effective_key,
                    &job_id,
                    &token,
                    "on fatal failure",
                )
                .await
                {
                    hb.idemp_committed.store(true, Ordering::SeqCst);
                }
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
                    if release_idempotency_token(
                        &idempotency_store,
                        &effective_key,
                        &job_id,
                        &token,
                        "before retryable nack; proceeding with nack",
                    )
                    .await
                    {
                        hb.idemp_committed.store(true, Ordering::SeqCst);
                    }
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
                    if release_idempotency_token(
                        &idempotency_store,
                        &effective_key,
                        &job_id,
                        &token,
                        "on dead letter",
                    )
                    .await
                    {
                        hb.idemp_committed.store(true, Ordering::SeqCst);
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
        {
            let _guard = self
                .admission_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.is_stopping.store(true, Ordering::SeqCst);
            self.shutdown_tx.send_replace(true);
        }
        // Why: use `tokio::time::Instant` so drain deadlines stay consistent with
        // Tokio sleeps under paused virtual time as well as wall-clock time.
        let deadline = tokio::time::Instant::now() + timeout;

        // 1. Wait for in-flight jobs to finish
        while self.in_flight_count.load(Ordering::SeqCst) > 0 {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            sleep(remaining.min(Duration::from_millis(10))).await;
        }

        // 2. Wait for polling, heartbeat, and recovery loops to exit
        while self.active_tasks.load(Ordering::SeqCst) > 0 {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            sleep(remaining.min(Duration::from_millis(10))).await;
        }

        !self.drain_dirty.load(Ordering::SeqCst)
    }

    /// Returns the number of currently executing jobs.
    pub fn in_flight_count(&self) -> u32 {
        self.in_flight_count.load(Ordering::Relaxed)
    }
}
