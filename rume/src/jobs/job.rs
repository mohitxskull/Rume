//! Typed background job declarations and execution contracts.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::jobs::error::JobError;
use crate::jobs::retry::RetryPolicy;

/// Pinned boxed future type alias for async job handler operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait defining a typed background job payload and retry semantics.
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Unique canonical name identifying this job type in queues and logs (e.g. "send_welcome_email").
    const NAME: &'static str;

    /// Execution retry policy for this job.
    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy::default()
    }

    /// Execution timeout for a single attempt.
    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    /// Optional idempotency key ensuring deduplication and at-most-once execution.
    fn idempotency_key(&self) -> Option<String> {
        None
    }

    /// Optional tenant identifier for multi-tenant queue and idempotency scoping.
    fn tenant_id(&self) -> Option<String> {
        None
    }

    /// Optional W3C TraceContext traceparent string propagating distributed trace identity.
    fn trace_context(&self) -> Option<String> {
        None
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Execution context provided to a job handler.
#[derive(Debug, Clone)]
pub struct JobContext {
    /// Unique identifier for this queued job instance.
    pub id: String,
    /// Current attempt number (starting at 1).
    pub attempt: u32,
    /// Maximum allowed attempts before DLQ.
    pub max_attempts: u32,
    /// Optional cancellation signal triggered during graceful worker shutdown.
    pub cancellation: Option<Arc<AtomicBool>>,
    /// Optional W3C TraceContext traceparent string.
    pub trace_context: Option<String>,
}

impl JobContext {
    /// Creates a new job execution context.
    pub fn new(id: impl Into<String>, attempt: u32, max_attempts: u32) -> Self {
        Self {
            id: id.into(),
            attempt,
            max_attempts,
            cancellation: None,
            trace_context: None,
        }
    }

    /// Attaches a cancellation token to the job context.
    pub fn with_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Attaches a W3C trace context string to the job context.
    pub fn with_trace_context(mut self, trace_context: impl Into<String>) -> Self {
        self.trace_context = Some(trace_context.into());
        self
    }

    /// Checks if the worker has requested graceful cancellation.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(false)
    }
}

/// Handler responsible for executing a specific background job type.
pub trait JobHandler<J: Job>: Send + Sync + 'static {
    /// Executes the job payload within the given context.
    fn handle<'a>(&'a self, ctx: &'a JobContext, job: J) -> BoxFuture<'a, Result<(), JobError>>;
}

/// Internal envelope for a serialized job queued in storage.
#[derive(Debug, Clone)]
pub struct QueuedJob {
    /// Unique job identifier.
    pub id: String,
    /// Job type identifier.
    pub name: String,
    /// Serialized JSON payload.
    pub payload: Vec<u8>,
    /// Current attempt count.
    pub attempt: u32,
    /// Job retry strategy.
    pub retry_policy: RetryPolicy,
    /// Execution timeout for a single attempt.
    pub timeout: Option<Duration>,
    /// Optional idempotency key for deduplication.
    pub idempotency_key: Option<String>,
    /// Optional tenant identifier for multi-tenant isolation.
    pub tenant_id: Option<String>,
    /// Identifier of the worker node actively executing this job.
    pub assigned_worker: Option<String>,
    /// Unique lease token fencing this claim attempt.
    pub lease_token: Option<String>,
    /// Earliest time when this job is eligible for execution.
    pub available_at: Instant,
    /// Lease duration granted for this execution attempt.
    pub lease_duration: Duration,
    /// Optional W3C TraceContext traceparent string.
    pub trace_context: Option<String>,
}
