//! Transactional outbox error types.

use thiserror::Error;

/// Errors that can occur during outbox storage, retrieval, or relay.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OutboxError {
    /// Outbox message serialization or deserialization failed.
    #[error("outbox serialization failed: {0}")]
    Serialization(String),

    /// Repository or database storage error.
    #[error("outbox repository error: {0}")]
    Repository(String),

    /// Event publishing error during outbox relay.
    #[error("outbox relay dispatch error: {0}")]
    Dispatch(String),

    /// The outbox relay has been stopped.
    #[error("outbox relay is stopped")]
    Stopped,

    /// The requested operation is not supported by this outbox repository backend.
    #[error("operation not supported by this outbox repository: {0}")]
    Unsupported(String),
}
