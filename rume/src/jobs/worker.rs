//! Background worker runner, handler registry, and graceful drain lifecycle.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use tokio::time::sleep;

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};
use crate::jobs::queue::JobQueue;

type ErasedHandler =
    Arc<dyn Fn(&JobContext, &[u8]) -> BoxFuture<'static, Result<(), JobError>> + Send + Sync>;

/// Registry mapping job names to their respective typed execution handlers.
#[derive(Default, Clone)]
pub struct JobRegistry {
    handlers: BTreeMap<String, ErasedHandler>,
}

impl Debug for JobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRegistry")
            .field("registered_jobs", &self.handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl JobRegistry {
    /// Creates an empty job registry.
    pub fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
        }
    }

    /// Registers a typed handler for a job type.
    pub fn register<J, H>(&mut self, handler: H)
    where
        J: Job,
        H: JobHandler<J>,
    {
        let handler = Arc::new(handler);
        let erased: ErasedHandler = Arc::new(move |ctx, payload| {
            let handler = Arc::clone(&handler);
            let ctx = ctx.clone();
            let payload = payload.to_vec();

            Box::pin(async move {
                let job: J = serde_json::from_slice(&payload)
                    .map_err(|e| JobError::Serialization(e.to_string()))?;
                handler.handle(&ctx, job).await
            })
        });

        self.handlers.insert(J::NAME.to_string(), erased);
    }

    /// Dispatches a raw queued job to its registered handler.
    pub async fn dispatch(&self, job: &QueuedJob) -> Result<(), JobError> {
        let Some(handler) = self.handlers.get(&job.name) else {
            return Err(JobError::UnknownJob(job.name.clone()));
        };

        let ctx = JobContext {
            id: job.id.clone(),
            attempt: job.attempt,
            max_attempts: job.retry_policy.max_attempts,
        };

        handler(&ctx, &job.payload).await
    }
}

/// Background worker managing queue polling, retries, and graceful shutdown drain.
pub struct JobWorker<Q: JobQueue> {
    queue: Arc<Q>,
    registry: Arc<JobRegistry>,
    is_stopping: Arc<AtomicBool>,
    in_flight_count: Arc<AtomicU32>,
}

impl<Q: JobQueue> Clone for JobWorker<Q> {
    fn clone(&self) -> Self {
        Self {
            queue: Arc::clone(&self.queue),
            registry: Arc::clone(&self.registry),
            is_stopping: Arc::clone(&self.is_stopping),
            in_flight_count: Arc::clone(&self.in_flight_count),
        }
    }
}

impl<Q: JobQueue> Debug for JobWorker<Q> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobWorker")
            .field("is_stopping", &self.is_stopping.load(Ordering::Relaxed))
            .field(
                "in_flight_jobs",
                &self.in_flight_count.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl<Q: JobQueue> JobWorker<Q> {
    /// Creates a new background worker.
    pub fn new(queue: Arc<Q>, registry: Arc<JobRegistry>) -> Self {
        Self {
            queue,
            registry,
            is_stopping: Arc::new(AtomicBool::new(false)),
            in_flight_count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Spawns the background execution loop into the current async runtime.
    pub fn start(&self) {
        let worker = self.clone();
        tokio::spawn(async move {
            worker.run_loop().await;
        });
    }

    /// Internal worker execution loop.
    async fn run_loop(&self) {
        while !self.is_stopping.load(Ordering::Relaxed) {
            match self.queue.dequeue().await {
                Ok(Some(job)) => {
                    self.in_flight_count.fetch_add(1, Ordering::SeqCst);
                    let queue = Arc::clone(&self.queue);
                    let registry = Arc::clone(&self.registry);
                    let in_flight = Arc::clone(&self.in_flight_count);

                    tokio::spawn(async move {
                        Self::execute_job(queue, registry, job).await;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                }
                Ok(None) => {
                    sleep(Duration::from_millis(25)).await;
                }
                Err(_) => {
                    sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    /// Executes a single dequeued job, handling acknowledgments, retries, and dead-lettering.
    async fn execute_job(queue: Arc<Q>, registry: Arc<JobRegistry>, job: QueuedJob) {
        let job_id = job.id.clone();
        match registry.dispatch(&job).await {
            Ok(()) => {
                let _ = queue.ack(&job_id).await;
            }
            Err(JobError::Fatal(msg)) => {
                let _ = queue.dead_letter(job, msg).await;
            }
            Err(err) => {
                // Retryable failure: check remaining attempts
                if let Some(backoff) = job.retry_policy.next_backoff(job.attempt) {
                    let _ = queue.nack(job, backoff).await;
                } else {
                    let reason = format!("max retries exceeded ({}) - error: {err}", job.attempt);
                    let _ = queue.dead_letter(job, reason).await;
                }
            }
        }
    }

    /// Gracefully requests worker shutdown and waits for in-flight jobs to complete within `timeout`.
    pub async fn drain(&self, timeout: Duration) -> bool {
        self.is_stopping.store(true, Ordering::SeqCst);
        let start = std::time::Instant::now();

        while self.in_flight_count.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= timeout {
                return false;
            }
            sleep(Duration::from_millis(20)).await;
        }

        true
    }

    /// Returns the number of currently executing jobs.
    pub fn in_flight_count(&self) -> u32 {
        self.in_flight_count.load(Ordering::Relaxed)
    }
}
