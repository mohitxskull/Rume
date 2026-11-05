//! Application event error types.

use thiserror::Error;

/// Errors that can occur during event dispatching or listener execution.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EventError {
    /// A listener failed during event execution.
    #[error("listener error on event '{event_type}': {message}")]
    ListenerError {
        event_type: &'static str,
        message: String,
    },

    /// An internal bus error occurred.
    #[error("event bus error: {0}")]
    Bus(String),
}
