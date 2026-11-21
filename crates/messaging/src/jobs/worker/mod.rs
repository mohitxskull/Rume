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
    // Why: `watch::Sender<bool>` latches shutdown and abort states (`false` -> `true`)
    // so no worker, heartbeat, or recovery loop can lose a wakeup if the signal arrives
    // while an async operation is in-flight or before `select!` is polled.
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
    abort_requested: Arc<AtomicBool>,
    abort_tx: Arc<tokio::sync::watch::Sender<bool>>,
    in_flight_count: Arc<AtomicU32>,
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
        self.is_stopping.store(true, Ordering::SeqCst);
        self.abort_requested.store(true, Ordering::SeqCst);
        let _ = self.abort_tx.send(true);
        let _ = self.shutdown_tx.send(true);
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
        self.is_stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(true);
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
