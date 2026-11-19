//! Multi-tenant error types.

use thiserror::Error;

use rume_foundation::error::AppError;

/// Errors that can occur during tenant resolution or tenant-scoped operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TenantError {
    /// No tenant could be resolved from the request.
    #[error("missing tenant identifier")]
    MissingTenant,

    /// The specified tenant was not found or is inactive.
    #[error("tenant not found: '{0}'")]
    TenantNotFound(String),

    /// The client supplied a malformed tenant identifier or host header.
    #[error("invalid tenant identifier: {0}")]
    InvalidIdentifier(String),

    /// The tenant resolver encountered an internal error (redacted in HTTP responses).
    #[error("tenant resolver internal error: {0}")]
    Internal(String),

    /// A tenant registration conflict occurred (e.g. duplicate slug or ID collision).
    #[error("tenant registration conflict: {0}")]
    Conflict(String),

    /// A tenant has exceeded its tiered quota limit.
    #[error("tenant '{tenant_id}' exceeded quota limit of {limit}")]
    QuotaExceeded { tenant_id: String, limit: usize },
}

impl From<TenantError> for AppError {
    fn from(err: TenantError) -> Self {
        match err {
            TenantError::MissingTenant => AppError::BadRequest {
                code: "MISSING_TENANT",
                message: "No tenant identifier was provided in the request".into(),
            },
            TenantError::TenantNotFound(slug) => AppError::Public {
                status: axum::http::StatusCode::NOT_FOUND,
                code: "TENANT_NOT_FOUND",
                message: format!("Tenant '{slug}' was not found"),
            },
            TenantError::InvalidIdentifier(msg) => AppError::BadRequest {
                code: "INVALID_TENANT_IDENTIFIER",
                message: msg,
            },
            TenantError::Internal(msg) => AppError::Internal {
                source: msg.into(),
                request_id: None,
            },
            TenantError::Conflict(msg) => AppError::Public {
                status: axum::http::StatusCode::CONFLICT,
                code: "TENANT_CONFLICT",
                message: msg,
            },
            TenantError::QuotaExceeded { tenant_id, limit } => AppError::Public {
                status: axum::http::StatusCode::TOO_MANY_REQUESTS,
                code: "TENANT_QUOTA_EXCEEDED",
                message: format!(
                    "Tenant '{tenant_id}' has exceeded its tiered quota limit of {limit}"
                ),
            },
        }
    }
}
