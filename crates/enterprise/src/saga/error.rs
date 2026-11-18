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
    Storage {
        /// Identifier of the saga that encountered the storage error, if known.
        #[serde(default)]
        saga_id: Option<String>,
        /// Description of the storage failure.
        message: String,
    },
}

impl SagaError {
    /// Returns the `saga_id` associated with a [`SagaError::Storage`] error, if present.
    pub fn saga_id(&self) -> Option<&str> {
        match self {
            Self::Storage { saga_id, .. } => saga_id.as_deref(),
            _ => None,
        }
    }
}
