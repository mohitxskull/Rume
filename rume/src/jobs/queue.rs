//! Job queue abstraction and in-memory queue with delay and dead-letter storage.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob};

/// Interface implemented by persistent and in-memory message brokers and queue backends.
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

#[derive(Debug, Default)]
struct MemoryQueueState {
    pending: VecDeque<QueuedJob>,
    in_flight: Vec<QueuedJob>,
    dead_letters: Vec<(QueuedJob, String)>,
    is_closed: bool,
}

/// An in-memory job queue for testing and local background processing.
#[derive(Debug, Clone, Default)]
pub struct MemoryJobQueue {
    state: Arc<RwLock<MemoryQueueState>>,
}

impl MemoryJobQueue {
    /// Creates a new in-memory job queue.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(MemoryQueueState::default())),
        }
    }

    /// Closes the queue, preventing further job submissions.
    pub fn close(&self) {
        if let Ok(mut state) = self.state.write() {
            state.is_closed = true;
        }
    }
}

impl JobQueue for MemoryJobQueue {
    fn enqueue_delayed<J: Job>(
        &self,
        job: J,
        delay: Duration,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        let name = J::NAME.to_string();
        let retry_policy = job.retry_policy();
        let timeout = job.timeout();
        let id = Uuid::new_v4().to_string();

        Box::pin(async move {
            let payload =
                serde_json::to_vec(&job).map_err(|e| JobError::Serialization(e.to_string()))?;

            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if state.is_closed {
                return Err(JobError::QueueClosed);
            }

            let queued = QueuedJob {
                id: id.clone(),
                name,
                payload,
                attempt: 0,
                retry_policy,
                timeout,
                available_at: Instant::now() + delay,
            };

            state.pending.push_back(queued);
            Ok(id)
        })
    }

    fn dequeue(&self) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            if state.is_closed && state.pending.is_empty() {
                return Ok(None);
            }

            let now = Instant::now();
            let mut found_idx = None;

            for (idx, job) in state.pending.iter().enumerate() {
                if job.available_at <= now {
                    found_idx = Some(idx);
                    break;
                }
            }

            if let Some(idx) = found_idx {
                let mut job = state.pending.remove(idx).unwrap();
                job.attempt += 1;
                state.in_flight.push(job.clone());
                Ok(Some(job))
            } else {
                Ok(None)
            }
        })
    }

    fn ack(&self, job_id: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let jid = job_id.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            state.in_flight.retain(|j| j.id != jid);
            Ok(())
        })
    }

    fn nack(&self, mut job: QueuedJob, delay: Duration) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            state.in_flight.retain(|j| j.id != job.id);
            job.available_at = Instant::now() + delay;
            state.pending.push_back(job);
            Ok(())
        })
    }

    fn dead_letter(&self, job: QueuedJob, reason: String) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;

            state.in_flight.retain(|j| j.id != job.id);
            state.dead_letters.push((job, reason));
            Ok(())
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            Ok(state.pending.len())
        })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            Ok(state.dead_letters.len())
        })
    }
}
