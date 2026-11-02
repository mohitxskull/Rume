//! Cache error types.

use thiserror::Error;

/// Errors that can occur during cache operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CacheError {
    /// Serialization or deserialization failure.
    #[error("cache serialization failed: {0}")]
    Serialization(String),

    /// Cache connection or backend failure.
    #[error("cache connection failed: {0}")]
    Connection(String),

    /// Cache backend has been closed.
    #[error("cache is closed")]
    Closed,

    /// General cache execution error.
    #[error("cache error: {0}")]
    Backend(String),

    /// Cache capacity or tenant quota exceeded.
    #[error("cache quota exceeded: {0}")]
    QuotaExceeded(String),
}
