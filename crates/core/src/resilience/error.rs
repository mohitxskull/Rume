//! Resilience middleware error types.

use thiserror::Error;

use crate::error::AppError;

/// Errors that can occur when resilience guards trip or limits are exceeded.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResilienceError {
    /// Circuit breaker is OPEN and refusing traffic to protect downstream services.
    #[error("circuit breaker is open")]
    CircuitOpen,

    /// Rate limit exceeded.
    #[error("rate limit exceeded (retry after {retry_after_secs}s)")]
    RateLimited { retry_after_secs: u64 },

    /// Concurrency limit reached; load shed to prevent resource exhaustion.
    #[error("concurrency limit exceeded: {current}/{max}")]
    LoadShed { current: usize, max: usize },

    /// Request execution timed out.
    #[error("request timed out")]
    Timeout,

    /// Underlying storage or communication backend failed.
    #[error("resilience backend error: {0}")]
    Backend(String),
}

impl From<ResilienceError> for AppError {
    fn from(err: ResilienceError) -> Self {
        match err {
            ResilienceError::Backend(msg) => {
                AppError::internal(format!("Resilience backend error: {msg}"))
            }
            ResilienceError::CircuitOpen => AppError::Public {
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
                code: "CIRCUIT_BREAKER_OPEN",
                message: "Service temporarily unavailable due to downstream protection".into(),
            },
            ResilienceError::RateLimited { retry_after_secs } => AppError::Public {
                status: axum::http::StatusCode::TOO_MANY_REQUESTS,
                code: "RATE_LIMIT_EXCEEDED",
                message: format!(
                    "Too many requests; please retry after {retry_after_secs} seconds"
                ),
            },
            ResilienceError::LoadShed { .. } => AppError::Public {
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
                code: "SHED_LOAD",
                message: "Server is temporarily at maximum concurrent capacity".into(),
            },
            ResilienceError::Timeout => AppError::Public {
                status: axum::http::StatusCode::GATEWAY_TIMEOUT,
                code: "TIMEOUT",
                message: "The request timed out".into(),
            },
        }
    }
}
