#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rume::jobs::{
    BoxFuture, Job, JobContext, JobError, JobHandler, JobQueue, JobRegistry, JobWorker,
    PostgresJobQueue, RetryPolicy,
};
use serde::{Deserialize, Serialize};
use testcontainers_modules::postgres::Postgres;
use tokio::time::sleep;

mod common;

async fn start_postgres_queue() -> (
    &'static testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    Arc<PostgresJobQueue>,
) {
    let node = common::containers::shared_postgres().await;
    let (_db_name, config) = common::containers::create_isolated_postgres_db().await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.url)
        .await
        .expect("pg pool");
    let queue = Arc::new(PostgresJobQueue::new(pool));
    queue.init_schema().await.expect("schema");
    (node, queue)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SendEmailJob {
    to: String,
    subject: String,
}

impl Job for SendEmailJob {
    const NAME: &'static str = "send_email";

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy::new()
            .with_max_attempts(3)
            .with_initial_backoff(Duration::from_millis(10))
    }
}

struct SendEmailHandler {
    attempts: Arc<AtomicU32>,
    fail_until_attempt: u32,
}

impl JobHandler<SendEmailJob> for SendEmailHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: SendEmailJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt < self.fail_until_attempt {
                Err(JobError::Retryable("smtp server busy".into()))
            } else {
                Ok(())
            }
        })
    }
}

#[tokio::test]
async fn job_success_and_retry_backoff() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();

    let attempts = Arc::new(AtomicU32::new(0));
    // Will fail on attempt 1, succeed on attempt 2
    registry.register(SendEmailHandler {
        attempts: Arc::clone(&attempts),
        fail_until_attempt: 2,
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    let job_id = queue
        .enqueue(SendEmailJob {
            to: "alice@example.com".into(),
            subject: "Welcome!".into(),
        })
        .await
        .unwrap();

    assert!(!job_id.is_empty());

    // Wait for processing and retry to complete
    for _ in 0..50 {
        if attempts.load(Ordering::SeqCst) >= 2 && queue.pending_count().await.unwrap() == 0 {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(queue.pending_count().await.unwrap(), 0);
    assert_eq!(queue.dead_letter_count().await.unwrap(), 0);

    let _ = worker.drain(Duration::from_millis(100)).await;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FatalFailingJob {
    reason: String,
}

impl Job for FatalFailingJob {
    const NAME: &'static str = "fatal_fail";
}

struct FatalHandler;

impl JobHandler<FatalFailingJob> for FatalHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: FatalFailingJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move { Err(JobError::Fatal(job.reason)) })
    }
}

#[tokio::test]
async fn fatal_failure_moves_immediately_to_dlq() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();
    registry.register(FatalHandler);

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    queue
        .enqueue(FatalFailingJob {
            reason: "corrupted payload schema".into(),
        })
        .await
        .unwrap();

    for _ in 0..50 {
        if queue.dead_letter_count().await.unwrap() >= 1 {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(queue.dead_letter_count().await.unwrap(), 1);
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    let _ = worker.drain(Duration::from_millis(100)).await;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlwaysFailingJob;

impl Job for AlwaysFailingJob {
    const NAME: &'static str = "always_fail";

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy::new()
            .with_max_attempts(2)
            .with_initial_backoff(Duration::from_millis(5))
    }
}

struct AlwaysFailHandler {
    attempts: Arc<AtomicU32>,
}

impl JobHandler<AlwaysFailingJob> for AlwaysFailHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: AlwaysFailingJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(JobError::Retryable("network unreachable".into()))
        })
    }
}

#[tokio::test]
async fn max_retries_exhausted_moves_to_dlq() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();

    let attempts = Arc::new(AtomicU32::new(0));
    registry.register(AlwaysFailHandler {
        attempts: Arc::clone(&attempts),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    queue.enqueue(AlwaysFailingJob).await.unwrap();

    for _ in 0..50 {
        if queue.dead_letter_count().await.unwrap() >= 1 {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }

    // Should have attempted 2 times (max_attempts: 2)
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(queue.dead_letter_count().await.unwrap(), 1);
    assert_eq!(queue.pending_count().await.unwrap(), 0);

    let _ = worker.drain(Duration::from_millis(100)).await;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlowJob {
    duration_ms: u64,
}

impl Job for SlowJob {
    const NAME: &'static str = "slow_job";
}

struct SlowJobHandler {
    finished: Arc<AtomicU32>,
}

impl JobHandler<SlowJob> for SlowJobHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: SlowJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            sleep(Duration::from_millis(job.duration_ms)).await;
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn worker_graceful_drain_waits_for_in_flight_jobs() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();

    let finished = Arc::new(AtomicU32::new(0));
    registry.register(SlowJobHandler {
        finished: Arc::clone(&finished),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    // Enqueue job that takes 80ms
    queue.enqueue(SlowJob { duration_ms: 80 }).await.unwrap();

    // Give it 20ms to be dequeued and in-flight
    sleep(Duration::from_millis(20)).await;
    assert_eq!(finished.load(Ordering::SeqCst), 0);

    // Initiate drain: should wait for in-flight job to finish
    let drained = worker.drain(Duration::from_millis(300)).await;
    assert!(drained);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PanickingJob;

impl Job for PanickingJob {
    const NAME: &'static str = "panicking_job";
}

struct PanickingJobHandler;

impl JobHandler<PanickingJob> for PanickingJobHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: PanickingJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            panic!("unhandled handler panic");
        })
    }
}

#[tokio::test]
async fn worker_panic_does_not_leak_in_flight_count() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();
    registry.register(PanickingJobHandler);

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    queue.enqueue(PanickingJob).await.unwrap();

    // Give time for worker to dequeue and panic
    sleep(Duration::from_millis(50)).await;

    // Drain should succeed without hanging because RAII guard decremented in_flight_count
    let drained = worker.drain(Duration::from_millis(200)).await;
    assert!(drained);

    // Durable queue invariant: panicked job must not be orphaned in 'in_flight'
    assert_eq!(queue.dead_letter_count().await.unwrap(), 1, "panicked job must be routed to DLQ");
    assert_eq!(queue.pending_count().await.unwrap(), 0);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimeoutJob;

impl Job for TimeoutJob {
    const NAME: &'static str = "timeout_job";

    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_millis(30))
    }

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy::new().with_max_attempts(1)
    }
}

struct TimeoutJobHandler;

impl JobHandler<TimeoutJob> for TimeoutJobHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        _job: TimeoutJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            sleep(Duration::from_millis(200)).await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn job_timeout_is_enforced() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();
    registry.register(TimeoutJobHandler);

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry));
    worker.start();

    queue.enqueue(TimeoutJob).await.unwrap();

    // Wait for execution and DLQ after timeout
    for _ in 0..50 {
        if queue.dead_letter_count().await.unwrap() >= 1 {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(queue.dead_letter_count().await.unwrap(), 1);
    let _ = worker.drain(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn worker_concurrency_is_bounded() {
    let (_node, queue) = start_postgres_queue().await;
    let mut registry = JobRegistry::new();

    let concurrent_running = Arc::new(AtomicU32::new(0));
    let max_observed = Arc::new(AtomicU32::new(0));

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct BoundJob;
    impl Job for BoundJob {
        const NAME: &'static str = "bound_job";
    }

    struct BoundHandler {
        running: Arc<AtomicU32>,
        max_seen: Arc<AtomicU32>,
    }

    impl JobHandler<BoundJob> for BoundHandler {
        fn handle<'a>(
            &'a self,
            _ctx: &'a JobContext,
            _job: BoundJob,
        ) -> BoxFuture<'a, Result<(), JobError>> {
            let running = Arc::clone(&self.running);
            let max_seen = Arc::clone(&self.max_seen);
            Box::pin(async move {
                let cur = running.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(cur, Ordering::SeqCst);
                sleep(Duration::from_millis(50)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    registry.register(BoundHandler {
        running: Arc::clone(&concurrent_running),
        max_seen: Arc::clone(&max_observed),
    });

    let worker = JobWorker::new(Arc::clone(&queue), Arc::new(registry)).with_concurrency(2);
    worker.start();

    for _ in 0..6 {
        queue.enqueue(BoundJob).await.unwrap();
    }

    let drained = worker.drain(Duration::from_millis(600)).await;
    assert!(drained);
    assert!(
        max_observed.load(Ordering::SeqCst) <= 2,
        "max concurrent observed was {}",
        max_observed.load(Ordering::SeqCst)
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MultiTenantDeduplicationJob {
    tenant: String,
    invoice_id: String,
}

impl Job for MultiTenantDeduplicationJob {
    const NAME: &'static str = "multi_tenant_dedup_job";

    fn idempotency_key(&self) -> Option<String> {
        Some(self.invoice_id.clone())
    }

    fn tenant_id(&self) -> Option<String> {
        Some(self.tenant.clone())
    }
}

#[tokio::test]
async fn cross_tenant_job_idempotency_isolation() {
    let (_node, queue) = start_postgres_queue().await;

    let job_a = MultiTenantDeduplicationJob {
        tenant: "tenant-a".into(),
        invoice_id: "inv-100".into(),
    };
    let job_b = MultiTenantDeduplicationJob {
        tenant: "tenant-b".into(),
        invoice_id: "inv-100".into(),
    };

    // Both tenants can enqueue a job with the identical idempotency key "inv-100" without collision
    let id_a = queue.enqueue(job_a.clone()).await.unwrap();
    let id_b = queue.enqueue(job_b.clone()).await.unwrap();
    assert_ne!(id_a, id_b, "different tenants must get distinct job IDs");

    // Idempotent retry within tenant-a returns the same ID
    let id_a_retry = queue.enqueue(job_a).await.unwrap();
    assert_eq!(id_a, id_a_retry, "tenant-a retry must resolve to existing job ID");
}
