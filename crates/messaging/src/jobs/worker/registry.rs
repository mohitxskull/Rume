use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::jobs::error::JobError;
use crate::jobs::job::{BoxFuture, Job, JobContext, JobHandler, QueuedJob};

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
        self.dispatch_with_cancellation(job, None).await
    }

    /// Dispatches a raw queued job to its registered handler with optional cancellation awareness.
    pub async fn dispatch_with_cancellation(
        &self,
        job: &QueuedJob,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> Result<(), JobError> {
        let Some(handler) = self.handlers.get(&job.name) else {
            return Err(JobError::UnknownJob(job.name.clone()));
        };

        let ctx = JobContext {
            id: job.id.clone(),
            attempt: job.attempt,
            max_attempts: job.retry_policy.max_attempts,
            cancellation,
            trace_context: job.trace_context.clone(),
        };

        let tc = job
            .trace_context
            .as_deref()
            .and_then(|s| {
                rume_observability::TraceContext::parse_serialized(s).map(|c| c.new_child())
            })
            .unwrap_or_else(rume_observability::TraceContext::generate);

        let span = tracing::info_span!(
            "job.dispatch",
            job.id = %job.id,
            job.name = %job.name,
            job.attempt = job.attempt,
            traceparent = %tc.traceparent,
        );

        tc.scope_future(span, async move {
            let fut = handler(&ctx, &job.payload);
            if let Some(timeout) = job.timeout {
                match tokio::time::timeout(timeout, fut).await {
                    Ok(res) => res,
                    Err(_) => Err(JobError::Timeout),
                }
            } else {
                fut.await
            }
        })
        .await
    }
}

/// RAII drop guard ensuring in-flight job counter is decremented even if the worker panics,
/// and notifying waiters when the last in-flight job finishes or cancels.
pub(super) struct InFlightGuard {
    pub(super) count: Arc<AtomicU32>,
    pub(super) zero_notify: Arc<tokio::sync::Notify>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.zero_notify.notify_waiters();
        }
    }
}
