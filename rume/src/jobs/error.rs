//! Background job error types.

use thiserror::Error;

/// Errors that can occur during job execution, queueing, or processing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum JobError {
    /// A retryable error occurred; the job should be retried according to its retry policy.
    #[error("job execution failed (retryable): {0}")]
    Retryable(String),

    /// A fatal error occurred; the job should fail immediately and move to the DLQ without retry.
    #[error("job execution failed (fatal): {0}")]
    Fatal(String),

    /// The job exceeded its execution timeout.
    #[error("job execution timed out")]
    Timeout,

    /// No handler was registered for this job type.
    #[error("unknown job type: '{0}'")]
    UnknownJob(String),

    /// Serialization or deserialization error.
    #[error("job serialization failed: {0}")]
    Serialization(String),

    /// The queue has been closed or stopped.
    #[error("job queue is closed")]
    QueueClosed,
}
