use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rume::jobs::{
    BoxFuture, Job, JobContext, JobError, JobHandler, JobQueue, JobRegistry, JobWorker,
    MemoryJobQueue, RetryPolicy,
};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

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
    let queue = Arc::new(MemoryJobQueue::new());
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
    let queue = Arc::new(MemoryJobQueue::new());
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
    let queue = Arc::new(MemoryJobQueue::new());
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
    let queue = Arc::new(MemoryJobQueue::new());
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
