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
}

/// Execution context provided to a job handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobContext {
    /// Unique identifier for this queued job instance.
    pub id: String,
    /// Current attempt number (starting at 1).
    pub attempt: u32,
    /// Maximum allowed attempts before DLQ.
    pub max_attempts: u32,
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
    /// Earliest time when this job is eligible for execution.
    pub available_at: Instant,
}
