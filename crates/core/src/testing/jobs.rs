//! In-memory job queue test double with assertion harness.

use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, QueuedJob};
use crate::jobs::queue::JobQueue;

/// An in-memory test double for [`JobQueue`] recording all enqueued jobs for assertions.
#[derive(Clone, Default)]
pub struct TestJobQueue {
    jobs: Arc<RwLock<Vec<QueuedJob>>>,
    acknowledged: Arc<RwLock<Vec<String>>>,
    dead_lettered: Arc<RwLock<Vec<(String, String)>>>,
}

impl Debug for TestJobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.jobs.read().map(|j| j.len()).unwrap_or(0);
        f.debug_struct("TestJobQueue")
            .field("enqueued_jobs_count", &count)
            .finish()
    }
}

impl TestJobQueue {
    /// Creates a new empty test job queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Asserts that at least one job of type `J` was enqueued, returning deserialized instances.
    pub fn assert_enqueued<J: Job>(&self) -> Vec<J> {
        let jobs = self.enqueued::<J>();
        assert!(
            !jobs.is_empty(),
            "expected at least one job '{}' to be enqueued, but none were recorded",
            J::NAME
        );
        jobs
    }

    /// Asserts that exactly `expected` jobs of type `J` were enqueued.
    pub fn assert_enqueued_count<J: Job>(&self, expected: usize) {
        let count = self.enqueued::<J>().len();
        assert_eq!(
            count,
            expected,
            "expected {} jobs of type '{}', but found {}",
            expected,
            J::NAME,
            count
        );
    }

    /// Returns whether any jobs of type `J` have been enqueued.
    pub fn has_enqueued<J: Job>(&self) -> bool {
        !self.enqueued::<J>().is_empty()
    }

    /// Deserializes and returns all enqueued jobs of type `J`.
    pub fn enqueued<J: Job>(&self) -> Vec<J> {
        if let Ok(jobs) = self.jobs.read() {
            jobs.iter()
                .filter(|q| q.name == J::NAME)
                .filter_map(|q| serde_json::from_slice::<J>(&q.payload).ok())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Returns raw queued job representations.
    pub fn enqueued_jobs(&self) -> Vec<QueuedJob> {
        self.jobs.read().map(|j| j.clone()).unwrap_or_default()
    }

    /// Clears all recorded jobs and acknowledgments.
    pub fn clear(&self) {
        if let Ok(mut j) = self.jobs.write() {
            j.clear();
        }
        if let Ok(mut a) = self.acknowledged.write() {
            a.clear();
        }
        if let Ok(mut d) = self.dead_lettered.write() {
            d.clear();
        }
    }
}

impl JobQueue for TestJobQueue {
    fn enqueue_delayed<J: Job>(
        &self,
        job: J,
        delay: Duration,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        let trace = job
            .trace_context()
            .or_else(|| crate::app::TraceContext::current().map(|tc| tc.to_string()));
        self.enqueue_delayed_with_trace(job, delay, trace)
    }

    fn enqueue_delayed_with_trace<J: Job>(
        &self,
        job: J,
        delay: Duration,
        trace_context: Option<String>,
    ) -> BoxFuture<'_, Result<String, JobError>> {
        let id = Uuid::new_v4().to_string();
        let payload = match serde_json::to_vec(&job) {
            Ok(p) => p,
            Err(e) => return Box::pin(async move { Err(JobError::Serialization(e.to_string())) }),
        };
        let effective_tc = trace_context
            .or_else(|| job.trace_context())
            .or_else(|| crate::app::TraceContext::current().map(|tc| tc.to_string()));
        let queued = QueuedJob {
            id: id.clone(),
            name: J::NAME.to_string(),
            payload,
            attempt: 0,
            retry_policy: job.retry_policy(),
            timeout: job.timeout(),
            idempotency_key: job.idempotency_key(),
            tenant_id: job.tenant_id(),
            assigned_worker: None,
            lease_token: None,
            available_at: Instant::now() + delay,
            lease_duration: Duration::from_secs(60),
            trace_context: effective_tc,
        };

        if let Ok(mut lock) = self.jobs.write() {
            lock.push(queued);
        }

        Box::pin(async move { Ok(id) })
    }

    fn dequeue_for(&self, worker_id: &str) -> BoxFuture<'_, Result<Option<QueuedJob>, JobError>> {
        let worker = worker_id.to_string();
        Box::pin(async move {
            let mut lock = self
                .jobs
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let now = Instant::now();
            for job in lock.iter_mut() {
                if job.available_at <= now && job.assigned_worker.is_none() {
                    job.assigned_worker = Some(worker.clone());
                    job.lease_token = Some(Uuid::new_v4().to_string());
                    job.attempt += 1;
                    return Ok(Some(job.clone()));
                }
            }
            Ok(None)
        })
    }

    fn reclaim_abandoned_jobs(
        &self,
        dead_workers: &[String],
    ) -> BoxFuture<'_, Result<usize, JobError>> {
        let workers = dead_workers.to_vec();
        Box::pin(async move {
            let mut lock = self
                .jobs
                .write()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let mut count = 0;
            for job in lock.iter_mut() {
                let should_reclaim = job
                    .assigned_worker
                    .as_ref()
                    .is_some_and(|w| workers.contains(w));
                if should_reclaim {
                    job.assigned_worker = None;
                    job.lease_token = None;
                    count += 1;
                }
            }
            Ok(count)
        })
    }

    fn ack(&self, job_id: &str, _lease_token: &str) -> BoxFuture<'_, Result<(), JobError>> {
        let id = job_id.to_string();
        Box::pin(async move {
            if let Ok(mut acked) = self.acknowledged.write() {
                acked.push(id.clone());
            }
            if let Ok(mut lock) = self.jobs.write() {
                lock.retain(|j| j.id != id);
            }
            Ok(())
        })
    }

    fn nack(
        &self,
        job: QueuedJob,
        _lease_token: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            let mut lock_guard = self.jobs.write().ok();
            if let Some(existing) = lock_guard
                .as_mut()
                .and_then(|l| l.iter_mut().find(|j| j.id == job.id))
            {
                existing.assigned_worker = None;
                existing.lease_token = None;
                existing.available_at = Instant::now() + delay;
            }
            Ok(())
        })
    }

    fn dead_letter(
        &self,
        job: QueuedJob,
        _lease_token: &str,
        reason: String,
    ) -> BoxFuture<'_, Result<(), JobError>> {
        Box::pin(async move {
            if let Ok(mut dlq) = self.dead_lettered.write() {
                dlq.push((job.id.clone(), reason));
            }
            if let Ok(mut lock) = self.jobs.write() {
                lock.retain(|j| j.id != job.id);
            }
            Ok(())
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        let count = self.jobs.read().map(|j| j.len()).unwrap_or(0);
        Box::pin(async move { Ok(count) })
    }

    fn dead_letter_count(&self) -> BoxFuture<'_, Result<usize, JobError>> {
        let count = self.dead_lettered.read().map(|d| d.len()).unwrap_or(0);
        Box::pin(async move { Ok(count) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct EmailJob {
        to: String,
        body: String,
    }

    impl Job for EmailJob {
        const NAME: &'static str = "send_email";
    }

    #[tokio::test]
    async fn test_job_queue_assert_enqueued() {
        let queue = TestJobQueue::new();
        let job_id = queue
            .enqueue(EmailJob {
                to: "test@example.com".into(),
                body: "hello world".into(),
            })
            .await
            .unwrap();
        assert!(!job_id.is_empty());

        assert!(queue.has_enqueued::<EmailJob>());
        queue.assert_enqueued_count::<EmailJob>(1);
        let jobs = queue.assert_enqueued::<EmailJob>();
        assert_eq!(jobs[0].to, "test@example.com");

        queue.clear();
        assert!(!queue.has_enqueued::<EmailJob>());
    }
}
