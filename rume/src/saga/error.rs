//! Saga execution errors.

use thiserror::Error;

/// Errors that can occur during saga step execution, compensation, or persistence.
#[derive(Debug, Error, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// A saga persistence or storage error occurred.
    #[error("saga storage error: {message}")]
    Storage { message: String },
}
