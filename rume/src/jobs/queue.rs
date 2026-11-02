//! Job queue abstraction for persistent message brokers.

use std::fmt::Debug;
use std::time::Duration;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob};

/// Interface implemented by persistent message brokers and queue backends.
pub trait JobQueue: Debug + Send + Sync + 'static {
    /// Enqueues a typed job for immediate execution.
    fn enqueue<J: Job>(&self, job: J) -> BoxFuture<'_, Result<String, JobError>> {
        self.enqueue_delayed(job, Duration::ZERO)
    }

    /// Enqueues a typed job with an initial execution delay.
    fn enqueue_delayed<J: Job>(
        &self,
        job: J,
        delay: Duration,
    ) -> BoxFuture<'_, Result<String, JobError>>;

    /// Dequeues the next available job whose `available_at` time has passed.
    fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>>;

    /// Dequeues the next available job, recording the worker identifier that claimed it.
    fn dequeue_for(&self, _worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        self.dequeue()
    }

    /// Reclaims jobs belonging to dead workers, returning them to the pending queue.
    fn reclaim_abandoned_jobs(
        &self,
        dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>>;

    /// Acknowledges successful execution, removing the job from the queue.
    fn ack(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>>;

    /// Re-queues a failed job after a backoff delay.
    fn nack(&self, job: QueuedJob, delay: Duration) -> BoxFuture<'_, Result<(), JobError>>;

    /// Moves a permanently failed job to the dead-letter queue (DLQ).
    fn dead_letter(&self, job: QueuedJob, reason: String) -> BoxFuture<'_, Result<(), JobError>>;

    /// Returns the number of currently pending jobs in the queue.
    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>>;

    /// Returns the number of jobs in the dead-letter queue.
    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>>;
}
