//! Saga execution errors.

use thiserror::Error;

/// Errors that can occur during saga step execution or compensation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SagaError {
    /// A forward action failed.
    #[error("step '{step}' execution failed: {message}")]
    ActionFailed { step: String, message: String },

    /// A compensation hook failed.
    #[error("step '{step}' compensation failed: {message}")]
    CompensationFailed { step: String, message: String },

    /// The saga coordinator was aborted.
    #[error("saga '{saga}' was aborted: {reason}")]
    Aborted { saga: String, reason: String },
}
