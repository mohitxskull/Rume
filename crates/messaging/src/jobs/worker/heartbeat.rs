//! Per-job lease-heartbeat plumbing used by [`JobWorker::execute_job`].
//!
//! [`JobWorker::execute_job`]: super::JobWorker::execute_job

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

use crate::jobs::error::JobError;
use crate::jobs::queue::JobQueue;
use crate::jobs::reliability::IdempotencyStore;

/// RAII guard that aborts the per-job lease-heartbeat task on drop.
struct HeartbeatAbortGuard(tokio::task::AbortHandle);

impl Drop for HeartbeatAbortGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// State shared between the spawned lease-heartbeat task and the finalization code in
/// [`JobWorker::execute_job`](super::JobWorker::execute_job).
pub(super) struct LeaseHeartbeat {
    /// Set by the heartbeat task when lease or idempotency ownership is lost.
    pub(super) lease_lost: Arc<AtomicBool>,
    /// Set once the distributed idempotency transition commits.
    pub(super) extend_idempotency: Arc<AtomicBool>,
    /// Set once the queue transition commits.
    pub(super) queue_committed: Arc<AtomicBool>,
    /// Set once the idempotency transition commits.
    pub(super) idemp_committed: Arc<AtomicBool>,
    /// Serializes finalization with in-flight heartbeat renewals.
    pub(super) tick_lock: Arc<Mutex<()>>,
    /// Cooperative-shutdown cancellation flag shared with the dispatched handler.
    pub(super) job_cancelled: Arc<AtomicBool>,
    cancel_tx: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
    abort_guard: HeartbeatAbortGuard,
}

impl LeaseHeartbeat {
    /// Requests the heartbeat task to stop and awaits its completion, preserving the
    /// original `execute_job` tail order: cancel, abort guard drop, then handle await.
    pub(super) async fn finish(self) {
        let _ = self.cancel_tx.send(());
        drop(self.abort_guard);
        let _ = self.handle.await;
    }
}

/// Inputs captured from `JobWorker::execute_job` when the inline heartbeat block was
/// extracted. Grouped into a single struct so the spawn function stays within
/// `clippy::too_many_arguments` without an `#[allow]`.
pub(super) struct LeaseHeartbeatParams<Q: JobQueue> {
    pub(super) queue: Arc<Q>,
    pub(super) job_id: String,
    pub(super) token: String,
    pub(super) stopping: Arc<AtomicBool>,
    pub(super) shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    pub(super) lost_leases: Arc<AtomicU64>,
    pub(super) idempotency_store: Option<Arc<dyn IdempotencyStore>>,
    pub(super) effective_key: Option<String>,
    pub(super) lease_duration: Duration,
}

/// Awaits a renewal future while independently observing graceful shutdown signals so a
/// slow renewal I/O call never delays setting `job_cancelled` for cooperative handlers.
async fn await_while_propagating_stop<F, T>(
    fut: F,
    stopping: &AtomicBool,
    job_cancelled: &AtomicBool,
    shutdown_rx: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(fut);
    if let Some(rx) = shutdown_rx.as_mut() {
        // Why: production job tasks supply `shutdown_rx`, so awaiting `rx.wait_for` wakes
        // immediately on shutdown without spawning periodic 20ms timer wakeups per in-flight job.
        loop {
            if job_cancelled.load(Ordering::Relaxed)
                || stopping.load(Ordering::Relaxed)
                || *rx.borrow()
            {
                if stopping.load(Ordering::Relaxed) || *rx.borrow() {
                    job_cancelled.store(true, Ordering::SeqCst);
                }
                return fut.await;
            }
            tokio::select! {
                biased;
                res = &mut fut => return res,
                _ = rx.wait_for(|&stopped| stopped) => {
                    job_cancelled.store(true, Ordering::SeqCst);
                }
            }
        }
    } else {
        // Fallback polling path only for receiver-free callers (e.g. unit tests calling
        // `spawn_lease_heartbeat` directly without a `watch::Receiver`).
        let mut stop_poll = tokio::time::interval(Duration::from_millis(20));
        loop {
            if job_cancelled.load(Ordering::Relaxed) || stopping.load(Ordering::Relaxed) {
                if stopping.load(Ordering::Relaxed) {
                    job_cancelled.store(true, Ordering::SeqCst);
                }
                return fut.await;
            }
            tokio::select! {
                biased;
                res = &mut fut => return res,
                _ = stop_poll.tick() => {
                    if stopping.load(Ordering::Relaxed) {
                        job_cancelled.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

/// Spawns the per-job lease-heartbeat task while the job executes (moved from
/// `JobWorker::execute_job`).
pub(super) fn spawn_lease_heartbeat<Q: JobQueue>(
    params: LeaseHeartbeatParams<Q>,
) -> LeaseHeartbeat {
    let LeaseHeartbeatParams {
        queue,
        job_id,
        token,
        stopping,
        mut shutdown_rx,
        lost_leases,
        idempotency_store,
        effective_key,
        lease_duration,
    } = params;
    let initial_stopped =
        stopping.load(Ordering::Relaxed) || shutdown_rx.as_ref().is_some_and(|rx| *rx.borrow());
    let job_cancelled = Arc::new(AtomicBool::new(initial_stopped));
    let lease_lost = Arc::new(AtomicBool::new(false));
    let extend_idempotency = Arc::new(AtomicBool::new(true));
    let queue_committed = Arc::new(AtomicBool::new(false));
    let idemp_committed = Arc::new(AtomicBool::new(false));
    let hb_tick_lock = Arc::new(Mutex::new(()));
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let hb_token = token.to_string();
    let hb_queue = Arc::clone(&queue);
    let hb_jid = job_id.to_string();
    let hb_cancelled = Arc::clone(&job_cancelled);
    let hb_lease_lost = Arc::clone(&lease_lost);
    let hb_extend_idempotency = Arc::clone(&extend_idempotency);
    let hb_queue_committed = Arc::clone(&queue_committed);
    let hb_idemp_committed = Arc::clone(&idemp_committed);
    let hb_tick_lock_task = Arc::clone(&hb_tick_lock);
    let hb_stopping = stopping;
    let hb_lost_leases = lost_leases;
    let hb_idemp_store = idempotency_store;
    let hb_idemp_key = effective_key;
    let hb_interval = (lease_duration / 3)
        .max(Duration::from_millis(20))
        .min(Duration::from_secs(20));
    // Why: bound each renewal I/O call by one lease duration (the actual ownership
    // horizon) so a hung DB/Redis connection cannot hold `hb_tick_lock` indefinitely
    // and stall finalization. A renewal that completes within the lease is not a
    // failure, even when it outlasts one heartbeat tick interval.
    let renewal_timeout = lease_duration.max(Duration::from_millis(50));
    let renew_extension = lease_duration.max(Duration::from_secs(60));

    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(hb_interval);
        interval.tick().await; // tick immediately
        let mut stop_check = shutdown_rx
            .is_none()
            .then(|| tokio::time::interval(Duration::from_millis(50)));
        loop {
            let not_yet_cancelled = !hb_cancelled.load(Ordering::Relaxed);
            tokio::select! {
                _ = &mut cancel_rx => break,
                _ = async {
                    if let Some(rx) = shutdown_rx.as_mut() {
                        let _ = rx.wait_for(|&stopped| stopped).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if not_yet_cancelled && shutdown_rx.is_some() => {
                    hb_cancelled.store(true, Ordering::SeqCst);
                }
                _ = async {
                    if let Some(timer) = stop_check.as_mut() {
                        timer.tick().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if not_yet_cancelled && stop_check.is_some() => {
                    if hb_stopping.load(Ordering::Relaxed) {
                        hb_cancelled.store(true, Ordering::SeqCst);
                    }
                }
                _ = interval.tick() => {
                    let _tick_guard = hb_tick_lock_task.lock().await;
                    let renew_res = match await_while_propagating_stop(
                        tokio::time::timeout(
                            renewal_timeout,
                            hb_queue.renew_lease(&hb_jid, &hb_token, renew_extension),
                        ),
                        &hb_stopping,
                        &hb_cancelled,
                        &mut shutdown_rx,
                    )
                    .await
                    {
                        Ok(res) => res,
                        Err(_) => Err(JobError::Fatal(format!(
                            "lease renewal timed out after {renewal_timeout:?}"
                        ))),
                    };
                    if let Err(e) = renew_res {
                        // Why: suppress renewal errors ONLY after the queue transition
                        // (`ack`/`nack`/`dead_letter`) has actually committed; any failure
                        // before commit is a genuine ownership loss that must set `lease_lost`.
                        if hb_queue_committed.load(Ordering::SeqCst) {
                            break;
                        }
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
                    {
                        let extend_res = match await_while_propagating_stop(
                            tokio::time::timeout(
                                renewal_timeout,
                                store.extend_lock_with_token(k, &hb_token, renew_extension),
                            ),
                            &hb_stopping,
                            &hb_cancelled,
                            &mut shutdown_rx,
                        )
                        .await
                        {
                            Ok(res) => res,
                            Err(_) => Err(JobError::Fatal(format!(
                                "idempotency lock renewal timed out after {renewal_timeout:?}"
                            ))),
                        };
                        if let Err(e) = extend_res {
                            // Why: suppress idempotency renewal failure only after idempotency
                            // completion/release has committed, never merely because finalization started.
                            if hb_idemp_committed.load(Ordering::SeqCst) {
                                break;
                            }
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
        }
    });

    let abort_guard = HeartbeatAbortGuard(heartbeat_handle.abort_handle());
    LeaseHeartbeat {
        lease_lost,
        extend_idempotency,
        queue_committed,
        idemp_committed,
        tick_lock: hb_tick_lock,
        job_cancelled,
        cancel_tx,
        handle: heartbeat_handle,
        abort_guard,
    }
}

/// Releases an idempotency token with a bounded wait so a stalled store cannot
/// indefinitely block worker state transitions.
///
/// Returns `true` if no token needed releasing or if the store confirmed release;
/// returns `false` on store error or timeout so callers only mark `idemp_committed`
/// after confirmed store success.
pub(super) async fn release_idempotency_token(
    store: &Option<Arc<dyn IdempotencyStore>>,
    key: &Option<String>,
    job_id: &str,
    token: &str,
    context: &str,
) -> bool {
    const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
    if let (Some(store), Some(key)) = (store, key) {
        match tokio::time::timeout(RELEASE_TIMEOUT, store.release_with_token(key, token)).await {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::error!(
                    job_id = %job_id,
                    key = %key,
                    error = %err,
                    "Failed to release idempotency token {context}"
                );
                false
            }
            Err(_) => {
                tracing::error!(
                    job_id = %job_id,
                    key = %key,
                    "Timed out releasing idempotency token {context}"
                );
                false
            }
        }
    } else {
        true
    }
}
