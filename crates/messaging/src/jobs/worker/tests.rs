use super::heartbeat::{LeaseHeartbeatParams, spawn_lease_heartbeat};
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
    defer_count: Arc<AtomicUsize>,
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

    fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        let worker_id = worker_id.to_string();
        Box::pin(async move {
            let _ = self.enter_tx.send(()).await;
            let _ = self.release_rx.lock().await.recv().await;
            let mut guard = self.job.lock().await;
            if let Some(mut j) = guard.take() {
                j.attempt += 1;
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

    fn defer(
        &self,
        mut job: QueuedJob,
        _lease_token: &str,
        _delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        self.defer_count.fetch_add(1, Ordering::SeqCst);
        job.attempt = job.attempt.saturating_sub(1);
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

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move { Ok(0) })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move { Ok(0) })
    }
}

#[tokio::test]
async fn worker_drain_while_dequeue_blocked_does_not_start_job_and_defers_without_burning_attempt()
{
    let (enter_tx, mut enter_rx) = mpsc::channel(2);
    let (release_tx, release_rx) = mpsc::channel(2);
    let defer_count = Arc::new(AtomicUsize::new(0));
    let nack_count = Arc::new(AtomicUsize::new(0));

    let payload_bytes = serde_json::to_vec(&BlockTestJob {
        data: "hello".into(),
    })
    .unwrap();

    let job = QueuedJob {
        id: "job-123".to_string(),
        name: BlockTestJob::NAME.to_string(),
        queue: "default".to_string(),
        priority: 0,
        payload: payload_bytes,
        attempt: 0,
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
        defer_count: Arc::clone(&defer_count),
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
    let drain_task = tokio::spawn(async move { drain_worker.drain(Duration::from_secs(2)).await });

    // Wait deterministically until `drain` sets `is_stopping`
    while !worker.is_stopping.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

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

    // 7. Verify the returned job was immediately deferred (not nack'd) and keeps attempt == 1
    assert_eq!(
        defer_count.load(Ordering::SeqCst),
        1,
        "unstarted job must be deferred immediately"
    );
    assert_eq!(
        nack_count.load(Ordering::SeqCst),
        0,
        "draining must call defer, not nack"
    );
    release_tx.send(()).await.expect("allow next dequeue");
    let next_claim = queue
        .dequeue_for("worker-next")
        .await
        .expect("dequeue after defer")
        .expect("deferred job must be available");
    assert_eq!(
        next_claim.attempt, 1,
        "deferring an unstarted job must not consume a retry attempt"
    );
}

#[tokio::test]
async fn test_worker_handle_shutdown_reports_panic() {
    let is_stopping = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (abort_tx, _) = tokio::sync::watch::channel(false);
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
        admission_lock: Arc::new(std::sync::Mutex::new(())),
        drain_dirty: Arc::new(AtomicBool::new(false)),
        shutdown_tx: Arc::new(shutdown_tx),
        abort_requested: Arc::new(AtomicBool::new(false)),
        abort_tx: Arc::new(abort_tx),
        in_flight_count,
        in_flight_zero: Arc::new(tokio::sync::Notify::new()),
        active_tasks,
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

#[tokio::test(start_paused = true)]
async fn test_worker_handle_timeout_aborts_tasks() {
    struct NotifyOnDrop(Arc<tokio::sync::Notify>);
    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    let is_stopping = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (abort_tx, _) = tokio::sync::watch::channel(false);
    let in_flight_count = Arc::new(AtomicU32::new(0));
    let active_tasks = Arc::new(AtomicU32::new(1));
    let task_dropped = Arc::new(tokio::sync::Notify::new());
    let dropped_wait = task_dropped.notified();
    tokio::pin!(dropped_wait);
    dropped_wait.as_mut().enable();

    // Long-running task that would suspend forever unless aborted
    let drop_notify = Arc::clone(&task_dropped);
    let long_task = tokio::spawn(async move {
        let _guard = NotifyOnDrop(drop_notify);
        std::future::pending::<()>().await;
    });
    let abort_handle = long_task.abort_handle();

    let mut handle = WorkerHandle {
        worker_task: long_task,
        heartbeat_task: None,
        recovery_task: None,
        is_stopping,
        admission_lock: Arc::new(std::sync::Mutex::new(())),
        drain_dirty: Arc::new(AtomicBool::new(false)),
        shutdown_tx: Arc::new(shutdown_tx),
        abort_requested: Arc::new(AtomicBool::new(false)),
        abort_tx: Arc::new(abort_tx),
        in_flight_count,
        in_flight_zero: Arc::new(tokio::sync::Notify::new()),
        active_tasks,
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

    // 2. consuming shutdown aborts remaining tasks on timeout and awaits cancellation
    let success = handle.shutdown(Duration::from_millis(50)).await;
    assert!(!success, "shutdown must return false on timeout");
    assert!(
        abort_handle.is_finished(),
        "shutdown() must wait for aborted task cancellation before returning"
    );

    tokio::time::timeout(Duration::from_secs(1), dropped_wait)
        .await
        .expect("aborted task drop guard must have fired");
}

#[tokio::test(start_paused = true)]
async fn test_worker_handle_try_shutdown_retry_after_heartbeat_timeout() {
    let worker_task = tokio::spawn(async {});
    let heartbeat_task = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(60)).await;
    });
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (abort_tx, _) = tokio::sync::watch::channel(false);

    let mut handle = WorkerHandle {
        worker_task,
        heartbeat_task: Some(heartbeat_task),
        recovery_task: None,
        is_stopping: Arc::new(AtomicBool::new(false)),
        admission_lock: Arc::new(std::sync::Mutex::new(())),
        drain_dirty: Arc::new(AtomicBool::new(false)),
        shutdown_tx: Arc::new(shutdown_tx),
        abort_requested: Arc::new(AtomicBool::new(false)),
        abort_tx: Arc::new(abort_tx),
        in_flight_count: Arc::new(AtomicU32::new(0)),
        in_flight_zero: Arc::new(tokio::sync::Notify::new()),
        active_tasks: Arc::new(AtomicU32::new(2)),
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
async fn test_purge_completed_default_implementation_returns_zero() {
    let (enter_tx, _) = mpsc::channel(1);
    let (_, release_rx) = mpsc::channel(1);
    let queue = BlockingTestQueue {
        job: Mutex::new(None),
        enter_tx,
        release_rx: Mutex::new(release_rx),
        defer_count: Arc::new(AtomicUsize::new(0)),
        nack_count: Arc::new(AtomicUsize::new(0)),
        ack_count: Arc::new(AtomicUsize::new(0)),
        dlq_count: Arc::new(AtomicUsize::new(0)),
        fail_renew: false,
    };

    // Exercises the default `JobQueue::purge_completed` implementation (real retention
    // purging is tested against PostgreSQL in `containers/postgres.rs`).
    let purged = queue
        .purge_completed(Duration::from_secs(3600))
        .await
        .unwrap();
    assert_eq!(purged, 0);
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

#[tokio::test(start_paused = true)]
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
        queue: "default".to_string(),
        priority: 0,
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
        defer_count: Arc::new(AtomicUsize::new(0)),
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

    fn release_with_token(&self, _key: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
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
        queue: "default".to_string(),
        priority: 0,
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
        defer_count: Arc::new(AtomicUsize::new(0)),
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
struct FailingRenewalIdempotencyStore {
    renewal_attempted: Arc<tokio::sync::Notify>,
}

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

    fn release_with_token(&self, _key: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move { Ok(()) })
    }

    fn extend_lock_with_token(
        &self,
        _key: &str,
        _token: &str,
        _extension: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        let attempted = Arc::clone(&self.renewal_attempted);
        Box::pin(async move {
            attempted.notify_one();
            Err(JobError::Fatal(
                "failed to extend idempotency key: token mismatch or lease expired".into(),
            ))
        })
    }
}

struct RenewalGatedHandler {
    gate: Arc<tokio::sync::Notify>,
}

impl JobHandler<SlowTestJob> for RenewalGatedHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: SlowTestJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            gate.notified().await;
            // Yield once so the heartbeat task's synchronous failure handling
            // (setting `lease_lost` and `job_cancelled`) finishes before handler exit.
            tokio::task::yield_now().await;
            Ok(())
        })
    }
}

#[tokio::test(start_paused = true)]
async fn test_worker_idempotency_renewal_failure_routes_to_dlq() {
    let (enter_tx, _enter_rx) = mpsc::channel(1);
    let (_release_tx, release_rx) = mpsc::channel(1);
    let nack_count = Arc::new(AtomicUsize::new(0));
    let ack_count = Arc::new(AtomicUsize::new(0));
    let dlq_count = Arc::new(AtomicUsize::new(0));
    let renewal_attempted = Arc::new(tokio::sync::Notify::new());

    let payload_bytes = serde_json::to_vec(&SlowTestJob).unwrap();
    let job = QueuedJob {
        id: "job-renew-fail".to_string(),
        name: SlowTestJob::NAME.to_string(),
        queue: "default".to_string(),
        priority: 0,
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
        defer_count: Arc::new(AtomicUsize::new(0)),
        nack_count: Arc::clone(&nack_count),
        ack_count: Arc::clone(&ack_count),
        dlq_count: Arc::clone(&dlq_count),
        fail_renew: false,
    });

    let mut registry = JobRegistry::new();
    registry.register(RenewalGatedHandler {
        gate: Arc::clone(&renewal_attempted),
    });

    let failing_store = Arc::new(FailingRenewalIdempotencyStore { renewal_attempted });

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

    fn release_with_token(&self, _key: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move { Ok(()) })
    }
}

#[tokio::test(start_paused = true)]
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
        queue: "default".to_string(),
        priority: 0,
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

    let renewal_attempted = Arc::new(tokio::sync::Notify::new());

    #[derive(Debug)]
    struct NotifyOnRenewQueue {
        inner: BlockingTestQueue,
        on_renew: Arc<tokio::sync::Notify>,
    }

    impl JobQueue for NotifyOnRenewQueue {
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            job: J,
            delay: Duration,
            trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            self.inner
                .enqueue_delayed_with_trace(job, delay, trace_context)
        }
        fn dequeue_for(
            &self,
            worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            self.inner.dequeue_for(worker_id)
        }
        fn reclaim_abandoned_jobs(
            &self,
            dead_workers: &[String],
        ) -> BoxFuture<'_, Result<usize, JobError>> {
            self.inner.reclaim_abandoned_jobs(dead_workers)
        }
        fn ack(&self, job_id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
            self.inner.ack(job_id, lease_token)
        }
        fn nack(
            &self,
            job: QueuedJob,
            lease_token: &str,
            delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            self.inner.nack(job, lease_token, delay)
        }
        fn dead_letter(
            &self,
            job: QueuedJob,
            lease_token: &str,
            reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            self.inner.dead_letter(job, lease_token, reason)
        }
        fn renew_lease(
            &self,
            job_id: &str,
            lease_token: &str,
            extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let on_renew = Arc::clone(&self.on_renew);
            let fut = self.inner.renew_lease(job_id, lease_token, extension);
            Box::pin(async move {
                let res = fut.await;
                on_renew.notify_one();
                res
            })
        }
        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            self.inner.pending_count()
        }
        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            self.inner.dead_letter_count()
        }
    }

    let queue = Arc::new(NotifyOnRenewQueue {
        inner: BlockingTestQueue {
            job: Mutex::new(None),
            enter_tx,
            release_rx: Mutex::new(release_rx),
            defer_count: Arc::new(AtomicUsize::new(0)),
            nack_count: Arc::clone(&nack_count),
            ack_count: Arc::clone(&ack_count),
            dlq_count: Arc::clone(&dlq_count),
            fail_renew: false,
        },
        on_renew: Arc::clone(&renewal_attempted),
    });

    let mut registry = JobRegistry::new();
    registry.register(RenewalGatedHandler {
        gate: renewal_attempted,
    });

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

    fn release_with_token(&self, key: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
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

    fn dequeue_for(&self, _worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
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
        queue: "default".to_string(),
        priority: 0,
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
    fn dequeue_for(&self, _worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
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

#[tokio::test(start_paused = true)]
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
        queue: "default".to_string(),
        priority: 0,
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
        queue: "default".to_string(),
        priority: 0,
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
    fn release_with_token(&self, key: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
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
    fn dequeue_for(&self, _worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
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

#[tokio::test(start_paused = true)]
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
        queue: "default".to_string(),
        priority: 0,
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
    fn release_with_token(&self, _key: &str, _token: &str) -> BoxFuture<'_, Result<(), JobError>> {
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
    fn dequeue_for(&self, _worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
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
        queue: "default".to_string(),
        priority: 0,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BurstOrHangJob {
    hang: bool,
}

impl Job for BurstOrHangJob {
    const NAME: &'static str = "burst_or_hang_job";
}

struct DropCanary(Arc<AtomicBool>);

impl Drop for DropCanary {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct BurstOrHangHandler {
    fast_completed: Arc<AtomicUsize>,
    hanging_started: Arc<tokio::sync::Notify>,
    hanging_dropped: Arc<AtomicBool>,
}

impl JobHandler<BurstOrHangJob> for BurstOrHangHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: BurstOrHangJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        let fast_completed = Arc::clone(&self.fast_completed);
        let hanging_started = Arc::clone(&self.hanging_started);
        let hanging_dropped = Arc::clone(&self.hanging_dropped);
        Box::pin(async move {
            if !job.hang {
                fast_completed.fetch_add(1, Ordering::SeqCst);
                return Ok(());
            }
            let _canary = DropCanary(hanging_dropped);
            hanging_started.notify_waiters();
            std::future::pending::<()>().await;
            Ok(())
        })
    }
}

#[derive(Debug)]
struct BurstTestQueue {
    jobs: std::sync::Mutex<std::collections::VecDeque<QueuedJob>>,
    acked: Arc<AtomicUsize>,
}

impl JobQueue for BurstTestQueue {
    fn enqueue_delayed_with_trace<J: Job>(
        &self,
        _job: J,
        _delay: Duration,
        _trace_context: Option<String>,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        Box::pin(async { Ok("id".into()) })
    }

    fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        let mut guard = self.jobs.lock().unwrap();
        let job = guard.pop_front().map(|mut j| {
            j.assigned_worker = Some(worker_id.to_string());
            j.lease_token = Some(format!("tok-{}", j.id));
            j
        });
        Box::pin(async move { Ok(job) })
    }

    fn reclaim_abandoned_jobs(
        &self,
        _dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async { Ok(0) })
    }

    fn ack(&self, _job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        self.acked.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
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
        _job: QueuedJob,
        _lease_token: &str,
        _reason: String,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async { Ok(()) })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async { Ok(self.jobs.lock().unwrap().len()) })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async { Ok(0) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_fast_completing_job_never_leaks_abort_handle() {
    let mut jobs = std::collections::VecDeque::with_capacity(1001);
    for idx in 0..1000 {
        jobs.push_back(QueuedJob {
            id: format!("fast-{idx}"),
            name: BurstOrHangJob::NAME.to_string(),
            queue: "default".to_string(),
            priority: 0,
            payload: serde_json::to_vec(&BurstOrHangJob { hang: false }).unwrap(),
            attempt: 1,
            retry_policy: RetryPolicy::new(),
            timeout: None,
            idempotency_key: None,
            tenant_id: None,
            assigned_worker: None,
            lease_token: None,
            available_at: Instant::now(),
            lease_duration: Duration::from_secs(30),
            trace_context: None,
        });
    }
    jobs.push_back(QueuedJob {
        id: "hang-job".to_string(),
        name: BurstOrHangJob::NAME.to_string(),
        queue: "default".to_string(),
        priority: 0,
        payload: serde_json::to_vec(&BurstOrHangJob { hang: true }).unwrap(),
        attempt: 1,
        retry_policy: RetryPolicy::new(),
        timeout: None,
        idempotency_key: None,
        tenant_id: None,
        assigned_worker: None,
        lease_token: None,
        available_at: Instant::now(),
        lease_duration: Duration::from_secs(30),
        trace_context: None,
    });

    let acked = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(BurstTestQueue {
        jobs: std::sync::Mutex::new(jobs),
        acked: Arc::clone(&acked),
    });

    let fast_completed = Arc::new(AtomicUsize::new(0));
    let hanging_started = Arc::new(tokio::sync::Notify::new());
    let hanging_dropped = Arc::new(AtomicBool::new(false));

    let mut registry = JobRegistry::new();
    registry.register(BurstOrHangHandler {
        fast_completed: Arc::clone(&fast_completed),
        hanging_started: Arc::clone(&hanging_started),
        hanging_dropped: Arc::clone(&hanging_dropped),
    });

    let worker = JobWorker::new(queue, Arc::new(registry)).with_concurrency(32);
    // Register the waiter before starting the worker so `notify_waiters()` cannot
    // fire before the future is armed.
    let started = hanging_started.notified();
    tokio::pin!(started);
    started.as_mut().enable();
    let handle = worker.start();

    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .expect("hanging job must start");

    let deadline = Instant::now() + Duration::from_secs(5);
    while acked.load(Ordering::SeqCst) < 1000
        || worker.in_flight_count() != 1
        || worker.join_set_len.load(Ordering::SeqCst) != 1
    {
        assert!(
            Instant::now() < deadline,
            "steady-state convergence failed: acked={}, in_flight={}, join_set_len={}",
            acked.load(Ordering::SeqCst),
            worker.in_flight_count(),
            worker.join_set_len.load(Ordering::SeqCst),
        );
        tokio::task::yield_now().await;
    }

    assert_eq!(fast_completed.load(Ordering::SeqCst), 1000);
    assert!(
        !hanging_dropped.load(Ordering::SeqCst),
        "hanging job should still be active before shutdown"
    );

    let clean = handle.shutdown(Duration::from_millis(50)).await;
    assert!(!clean, "shutdown must time out on hanging job");
    assert!(
        hanging_dropped.load(Ordering::SeqCst),
        "hanging job must be aborted and dropped on shutdown timeout"
    );
    assert_eq!(worker.in_flight_count(), 0);
    assert_eq!(worker.join_set_len.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn in_flight_idempotency_loss_before_ack_routes_to_dead_letter() {
    #[derive(Debug)]
    struct GatedFailExtendStore {
        extend_entered: Arc<tokio::sync::Notify>,
        release_extend: Arc<tokio::sync::Notify>,
    }

    impl IdempotencyStore for GatedFailExtendStore {
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
            _key: &str,
            _token: &str,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }

        fn extend_lock_with_token(
            &self,
            _key: &str,
            _token: &str,
            _ttl: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let entered = Arc::clone(&self.extend_entered);
            let release = Arc::clone(&self.release_extend);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Err(JobError::Fatal("idempotency token reassigned".into()))
            })
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct GatedIdempJob;

    impl Job for GatedIdempJob {
        const NAME: &'static str = "gated_idemp_job";
    }

    #[derive(Debug)]
    struct GatedHandler {
        extend_entered: Arc<tokio::sync::Notify>,
        release_extend: Arc<tokio::sync::Notify>,
    }

    impl JobHandler<GatedIdempJob> for GatedHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: GatedIdempJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let entered = Arc::clone(&self.extend_entered);
            let release = Arc::clone(&self.release_extend);
            Box::pin(async move {
                // Wait until the heartbeat task has entered `extend_lock_with_token`.
                entered.notified().await;
                // Release the failing renewal concurrently with handler completion;
                // `execute_job` must wait for the in-flight renewal to settle and
                // dead-letter the job instead of suppressing the error and acking.
                release.notify_one();
                Ok(())
            })
        }
    }

    let extend_entered = Arc::new(tokio::sync::Notify::new());
    let release_extend = Arc::new(tokio::sync::Notify::new());

    let (enter_tx, _enter_rx) = mpsc::channel(1);
    let (_release_tx, release_rx) = mpsc::channel(1);
    let nack_count = Arc::new(AtomicUsize::new(0));
    let ack_count = Arc::new(AtomicUsize::new(0));
    let dlq_count = Arc::new(AtomicUsize::new(0));

    let job = QueuedJob {
        id: "job-gated-idemp".to_string(),
        name: GatedIdempJob::NAME.to_string(),
        queue: "default".to_string(),
        priority: 0,
        payload: serde_json::to_vec(&GatedIdempJob).unwrap(),
        attempt: 1,
        retry_policy: RetryPolicy::new(),
        timeout: Some(Duration::from_secs(5)),
        idempotency_key: Some("k-gated".into()),
        tenant_id: None,
        assigned_worker: Some("worker-1".into()),
        lease_token: Some("token-gated".into()),
        available_at: Instant::now(),
        lease_duration: Duration::from_millis(60),
        trace_context: None,
    };

    let queue = Arc::new(BlockingTestQueue {
        job: Mutex::new(None),
        enter_tx,
        release_rx: Mutex::new(release_rx),
        defer_count: Arc::new(AtomicUsize::new(0)),
        nack_count: Arc::clone(&nack_count),
        ack_count: Arc::clone(&ack_count),
        dlq_count: Arc::clone(&dlq_count),
        fail_renew: false,
    });

    let mut registry = JobRegistry::new();
    registry.register(GatedHandler {
        extend_entered: Arc::clone(&extend_entered),
        release_extend: Arc::clone(&release_extend),
    });

    let store: Arc<dyn IdempotencyStore> = Arc::new(GatedFailExtendStore {
        extend_entered,
        release_extend,
    });

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

    assert_eq!(
        dlq_count.load(Ordering::SeqCst),
        1,
        "job must be dead-lettered when in-flight idempotency renewal fails before ACK"
    );
    assert_eq!(
        ack_count.load(Ordering::SeqCst),
        0,
        "job must not be acked when in-flight idempotency renewal fails before ACK"
    );
}

#[tokio::test]
async fn stalled_heartbeat_renewal_times_out_and_does_not_hang_finalization() {
    #[derive(Debug)]
    struct StalledRenewQueue {
        renew_entered: Arc<tokio::sync::Notify>,
        ack_count: Arc<AtomicUsize>,
        dlq_count: Arc<AtomicUsize>,
    }

    impl JobQueue for StalledRenewQueue {
        fn enqueue<J: Job>(&self, _job: J) -> BoxFuture<'_, Result<String, JobError>> {
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
        fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            Box::pin(async { Ok(None) })
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
        fn renew_lease(
            &self,
            _job_id: &str,
            _lease_token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let entered = Arc::clone(&self.renew_entered);
            Box::pin(async move {
                entered.notify_one();
                // Simulate a hung DB connection that never responds.
                std::future::pending::<()>().await;
                Ok(())
            })
        }
        fn ack(&self, _job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
            self.ack_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
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
            _job: QueuedJob,
            _lease_token: &str,
            _reason: String,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            self.dlq_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
        fn defer(
            &self,
            _job: QueuedJob,
            _lease_token: &str,
            _delay: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            Box::pin(async { Ok(()) })
        }
        fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(0) })
        }
        fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
            Box::pin(async { Ok(self.dlq_count.load(Ordering::SeqCst)) })
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct StalledRenewJob;
    impl Job for StalledRenewJob {
        const NAME: &'static str = "stalled_renew_job";
    }

    #[derive(Debug)]
    struct WaitForRenewHandler(Arc<tokio::sync::Notify>);
    impl JobHandler<StalledRenewJob> for WaitForRenewHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: StalledRenewJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let entered = Arc::clone(&self.0);
            Box::pin(async move {
                entered.notified().await;
                Ok(())
            })
        }
    }

    let renew_entered = Arc::new(tokio::sync::Notify::new());
    let ack_count = Arc::new(AtomicUsize::new(0));
    let dlq_count = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(StalledRenewQueue {
        renew_entered: Arc::clone(&renew_entered),
        ack_count: Arc::clone(&ack_count),
        dlq_count: Arc::clone(&dlq_count),
    });

    let mut registry = JobRegistry::new();
    registry.register(WaitForRenewHandler(renew_entered));

    let job = QueuedJob {
        id: "job-stalled-renew".to_string(),
        name: StalledRenewJob::NAME.to_string(),
        queue: "default".to_string(),
        priority: 0,
        payload: serde_json::to_vec(&StalledRenewJob).unwrap(),
        attempt: 1,
        retry_policy: RetryPolicy::new(),
        timeout: Some(Duration::from_secs(5)),
        idempotency_key: None,
        tenant_id: None,
        assigned_worker: Some("worker-1".into()),
        lease_token: Some("token-stalled".into()),
        available_at: Instant::now(),
        lease_duration: Duration::from_millis(60),
        trace_context: None,
    };

    tokio::time::timeout(
        Duration::from_secs(2),
        JobWorker::execute_job(
            queue,
            Arc::new(registry),
            job,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        ),
    )
    .await
    .expect("stalled heartbeat renewal must time out instead of hanging finalization");

    assert_eq!(dlq_count.load(Ordering::SeqCst), 1);
    assert_eq!(ack_count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn slow_renewal_within_lease_does_not_count_as_lost_lease() {
    #[derive(Debug)]
    struct SlowRenewQueue {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        completed: Arc<tokio::sync::Notify>,
        ok_count: Arc<AtomicUsize>,
    }

    impl JobQueue for SlowRenewQueue {
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
        fn renew_lease(
            &self,
            _job_id: &str,
            _lease_token: &str,
            _extension: Duration,
        ) -> BoxFuture<'_, Result<(), JobError>> {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            let completed = Arc::clone(&self.completed);
            let ok_count = Arc::clone(&self.ok_count);
            Box::pin(async move {
                if ok_count.load(Ordering::SeqCst) == 0 {
                    entered.notify_one();
                    release.notified().await;
                }
                ok_count.fetch_add(1, Ordering::SeqCst);
                completed.notify_one();
                Ok(())
            })
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

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(tokio::sync::Notify::new());
    let ok_count = Arc::new(AtomicUsize::new(0));

    let entered_wait = entered.notified();
    tokio::pin!(entered_wait);
    entered_wait.as_mut().enable();

    let completed_wait = completed.notified();
    tokio::pin!(completed_wait);
    completed_wait.as_mut().enable();

    let lost_leases = Arc::new(AtomicU64::new(0));
    let hb = spawn_lease_heartbeat(LeaseHeartbeatParams {
        queue: Arc::new(SlowRenewQueue {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            completed: Arc::clone(&completed),
            ok_count: Arc::clone(&ok_count),
        }),
        job_id: "job-slow-renew".to_string(),
        token: "token-slow-renew".to_string(),
        stopping: Arc::new(AtomicBool::new(false)),
        shutdown_rx: None,
        lost_leases: Arc::clone(&lost_leases),
        idempotency_store: None,
        effective_key: None,
        lease_duration: Duration::from_millis(600),
    });

    // Wait for renewal entry, advance beyond one ~200ms tick interval but less than
    // the 600ms lease duration, then release the renewal and prove completion before
    // checking loss indicators.
    tokio::time::timeout(Duration::from_secs(1), entered_wait)
        .await
        .expect("renewal must enter");
    tokio::time::advance(Duration::from_millis(350)).await;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), completed_wait)
        .await
        .expect("slow renewal must complete");
    drop(hb.tick_lock.lock().await);

    assert!(
        ok_count.load(Ordering::SeqCst) >= 1,
        "slow renewal must have completed"
    );
    assert!(
        !hb.lease_lost.load(Ordering::SeqCst),
        "a renewal that completes within the lease must not be reported as lease loss"
    );
    assert_eq!(
        lost_leases.load(Ordering::SeqCst),
        0,
        "a renewal that completes within the lease must not increment lost_leases"
    );

    hb.finish().await;
}

#[tokio::test]
async fn job_worker_enforces_per_queue_concurrency_cap() {
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct CappedJob {
        idx: usize,
    }

    impl Job for CappedJob {
        const NAME: &'static str = "capped_queue_job";

        fn queue(&self) -> &str {
            "emails"
        }
    }

    struct ConcurrencyProbeHandler {
        active: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
        done_tx: mpsc::Sender<()>,
    }

    impl JobHandler<CappedJob> for ConcurrencyProbeHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: CappedJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let active = Arc::clone(&self.active);
            let max_observed = Arc::clone(&self.max_observed);
            let done_tx = self.done_tx.clone();
            Box::pin(async move {
                let prev = active.fetch_add(1, Ordering::SeqCst);
                let current = prev + 1;
                max_observed.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                let _ = done_tx.send(()).await;
                Ok(())
            })
        }
    }

    #[derive(Debug)]
    struct MultiJobQueue {
        jobs: Mutex<Vec<QueuedJob>>,
    }

    impl JobQueue for MultiJobQueue {
        fn enqueue_delayed_with_trace<J: Job>(
            &self,
            _job: J,
            _delay: Duration,
            _trace_context: Option<String>,
        ) -> BoxFuture<'_, Result<String, JobError>> {
            Box::pin(async move { Ok("id".into()) })
        }

        fn dequeue_for(
            &self,
            worker_id: &str,
        ) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
            let wid = worker_id.to_string();
            Box::pin(async move {
                let mut guard = self.jobs.lock().await;
                if guard.is_empty() {
                    Ok(None)
                } else {
                    let mut job = guard.remove(0);
                    job.attempt += 1;
                    job.assigned_worker = Some(wid);
                    job.lease_token = Some("tok".into());
                    Ok(Some(job))
                }
            })
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

    let queued: Vec<QueuedJob> = (0..6)
        .map(|idx| QueuedJob {
            id: format!("capped-{idx}"),
            name: CappedJob::NAME.to_string(),
            queue: "emails".to_string(),
            priority: 0,
            payload: serde_json::to_vec(&CappedJob { idx }).unwrap(),
            attempt: 0,
            retry_policy: RetryPolicy::default(),
            timeout: Some(Duration::from_secs(5)),
            idempotency_key: None,
            tenant_id: None,
            assigned_worker: None,
            lease_token: None,
            available_at: Instant::now(),
            lease_duration: Duration::from_secs(30),
            trace_context: None,
        })
        .collect();

    let queue = Arc::new(MultiJobQueue {
        jobs: Mutex::new(queued),
    });
    let active = Arc::new(AtomicUsize::new(0));
    let max_observed = Arc::new(AtomicUsize::new(0));
    let (done_tx, mut done_rx) = mpsc::channel(10);

    let mut registry = JobRegistry::new();
    registry.register(ConcurrencyProbeHandler {
        active: Arc::clone(&active),
        max_observed: Arc::clone(&max_observed),
        done_tx,
    });

    let worker = JobWorker::with_concurrency_limit(queue, Arc::new(registry), 10)
        .with_queue_concurrency("emails", 2);
    let handle = worker.start();

    for _ in 0..6 {
        tokio::time::timeout(Duration::from_secs(2), done_rx.recv())
            .await
            .expect("job completed in time")
            .expect("channel open");
    }

    assert!(handle.shutdown(Duration::from_secs(2)).await);
    assert_eq!(max_observed.load(Ordering::SeqCst), 2);
}
