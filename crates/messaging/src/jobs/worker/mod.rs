//! Background worker runner, handler registry, and graceful drain lifecycle.
//!
//! The module tree is split into:
//! - [`registry`]: [`JobRegistry`] — typed job handler registration and dispatch.
//! - [`runner`]: [`JobWorker`] — the queue polling, job execution, and graceful drain loop.
//! - [`heartbeat`]: the per-job lease-heartbeat plumbing used by `JobWorker::execute_job`.
//!
//! [`JobRegistry`], [`JobWorker`], [`WorkerStatus`], and [`WorkerHandle`] are re-exported here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

#[cfg(test)]
use crate::jobs::error::JobError;
#[cfg(test)]
use crate::jobs::queue::JobQueue;
#[cfg(test)]
use crate::jobs::reliability::{IdempotencyStatus, IdempotencyStore};

#[cfg(test)]
use std::sync::atomic::AtomicU64;

#[cfg(test)]
use crate::jobs::reliability::scoped_idempotency_key;

mod heartbeat;
mod registry;
mod runner;

#[cfg(test)]
mod tests;

pub use registry::JobRegistry;
pub use runner::{JobWorker, WorkerStatus};

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
    // Why: serializes the `is_stopping` transition against post-dequeue job admission
    // in `run_loop` so no new job can start after draining begins.
    admission_lock: Arc<std::sync::Mutex<()>>,
    // Why: set when deferring an unstarted job during drain fails, so `try_shutdown`
    // and `drain` report `false` (incomplete drain) rather than silently claiming success.
    drain_dirty: Arc<AtomicBool>,
    // Why: `watch::Sender<bool>` latches shutdown and abort states (`false` -> `true`)
    // so no worker, heartbeat, or recovery loop can lose a wakeup if the signal arrives
    // while an async operation is in-flight or before `select!` is polled.
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
    abort_requested: Arc<AtomicBool>,
    abort_tx: Arc<tokio::sync::watch::Sender<bool>>,
    in_flight_count: Arc<AtomicU32>,
    in_flight_zero: Arc<tokio::sync::Notify>,
    active_tasks: Arc<AtomicU32>,
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
        {
            let _guard = self
                .admission_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.is_stopping.store(true, Ordering::SeqCst);
            let _ = self.shutdown_tx.send(true);
        }
        self.abort_requested.store(true, Ordering::SeqCst);
        let _ = self.abort_tx.send(true);
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
    /// Does not consume `self`, allowing the caller to inspect task status or retry if shutdown times out.
    pub async fn try_shutdown(&mut self, timeout: Duration) -> bool {
        {
            let _guard = self
                .admission_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.is_stopping.store(true, Ordering::SeqCst);
            let _ = self.shutdown_tx.send(true);
        }
        // Why: use `tokio::time::Instant` so shutdown deadlines stay consistent with
        // Tokio sleeps/timeouts under paused virtual time as well as wall-clock time.
        let deadline = tokio::time::Instant::now() + timeout;

        // 1. Wait for in-flight job handlers to complete
        while self.in_flight_count.load(Ordering::SeqCst) > 0 {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
        }

        // 2. Await the worker execution loop (caching only an observed join outcome)
        let worker_ok = match self.worker_result {
            Some(ok) => ok,
            None => {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
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
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
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
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
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

        !self.drain_dirty.load(Ordering::SeqCst)
    }

    /// Signals the worker to stop, waits for in-flight jobs to complete, and awaits all tasks.
    ///
    /// If shutdown times out, all remaining background tasks are aborted and joined within a
    /// bounded cleanup phase (250 ms) so cancellation completes before dependencies are torn
    /// down. If a task still has not finished after the bounded cleanup phase (e.g. blocked in
    /// non-cancellable I/O), cancellation remains pending on the runtime when `shutdown` returns.
    ///
    /// Returns `true` if cleanly shut down within `timeout`, or `false` if aborted after timeout
    /// expiration or if any task failed/panicked or a drain deferral failed.
    pub async fn shutdown(mut self, timeout: Duration) -> bool {
        let clean = self.try_shutdown(timeout).await;
        if !clean {
            // Why: signal `run_loop` to execute `JoinSet::shutdown().await` before falling back
            // to `abort_all()`, because `worker_task.abort()` only drops child tasks
            // asynchronously via `JoinSet::drop` and can leave `InFlightGuard` decrements
            // incomplete when `shutdown()` returns; `abort_all()` remains the hard fallback if
            // `run_loop` is blocked inside `dequeue_for`.
            self.abort_requested.store(true, Ordering::SeqCst);
            let _ = self.abort_tx.send(true);
            if self.worker_result.is_none()
                && self.in_flight_count.load(Ordering::SeqCst) > 0
                && tokio::time::timeout(Duration::from_millis(50), &mut self.worker_task)
                    .await
                    .is_ok()
            {
                self.worker_result = Some(false);
            }
            self.abort_all();

            // Why: `JoinHandle::abort` only schedules cancellation; join any still-unjoined
            // tasks within a bounded cleanup phase so callers do not tear down pools/services
            // while an aborted worker/heartbeat/recovery task is still dropping.
            const ABORT_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);
            let cleanup_deadline = tokio::time::Instant::now() + ABORT_CLEANUP_TIMEOUT;
            if self.worker_result.is_none() {
                let rem = cleanup_deadline.saturating_duration_since(tokio::time::Instant::now());
                let _ = tokio::time::timeout(rem, &mut self.worker_task).await;
            }
            if self.heartbeat_result.is_none()
                && let Some(hb) = self.heartbeat_task.as_mut()
            {
                let rem = cleanup_deadline.saturating_duration_since(tokio::time::Instant::now());
                let _ = tokio::time::timeout(rem, hb).await;
            }
            if self.recovery_result.is_none()
                && let Some(rec) = self.recovery_task.as_mut()
            {
                let rem = cleanup_deadline.saturating_duration_since(tokio::time::Instant::now());
                let _ = tokio::time::timeout(rem, rec).await;
            }
            // Why: when `worker_task` is hard-aborted while blocked in `dequeue_for`, joining
            // `worker_task` only confirms its `JoinSet` was dropped, not that spawned job tasks
            // finished cancelling. Wait for `in_flight_count` to hit zero within the remaining
            // cleanup budget so aborted job tasks drop before callers tear down resources.
            while self.in_flight_count.load(Ordering::SeqCst) > 0 {
                let rem = cleanup_deadline.saturating_duration_since(tokio::time::Instant::now());
                if rem.is_zero() {
                    break;
                }
                let notified = self.in_flight_zero.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.in_flight_count.load(Ordering::SeqCst) == 0 {
                    break;
                }
                if tokio::time::timeout(rem, notified).await.is_err() {
                    break;
                }
            }
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
