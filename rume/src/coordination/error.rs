//! Coordination error types.

use thiserror::Error;

/// Errors that can occur during distributed locking or leader election.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoordinationError {
    /// Failed to acquire or renew a lock.
    #[error("failed to acquire lock for '{resource}': {reason}")]
    LockUnavailable { resource: String, reason: String },

    /// The lock held has expired or been revoked.
    #[error("lock '{resource}' has expired")]
    LockExpired { resource: String },

    /// An internal coordinator error occurred.
    #[error("coordinator error: {0}")]
    Backend(String),
}
