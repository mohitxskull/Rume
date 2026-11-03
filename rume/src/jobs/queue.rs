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

    /// Acknowledges successful execution, transitioning the job to completed status.
    fn ack(&self, job_id: &str, lease_token: &str) -> BoxFuture<'_, Result<(), JobError>>;

    /// Re-queues a failed job after a backoff delay, fenced by worker lease token.
    fn nack(
        &self,
        job: QueuedJob,
        lease_token: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>>;

    /// Moves a permanently failed job to the dead-letter queue (DLQ), fenced by worker lease token.
    fn dead_letter(
        &self,
        job: QueuedJob,
        lease_token: &str,
        reason: String,
    ) -> BoxFuture<'_, Result<(), JobError>>;

    /// Extends the exclusive claim lease of an actively running job.
    fn renew_lease(
        &self,
        _job_id: &str,
        _lease_token: &str,
        _extension: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move { Ok(()) })
    }

    /// Returns the number of currently pending jobs in the queue.
    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>>;

    /// Returns the number of jobs in the dead-letter queue.
    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>>;

    /// Lists up to `limit` dead-lettered jobs for administrative inspection.
    fn list_dead_letters(
        &self,
        _limit: usize,
    ) -> BoxFuture<'_, Result<Vec<QueuedJob>, JobError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    /// Replays a dead-lettered job by resetting its status back to `pending`.
    fn replay_dead_letter(
        &self,
        _job_id: &str,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move { Ok(()) })
    }
}
