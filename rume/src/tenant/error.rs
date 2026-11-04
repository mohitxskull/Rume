//! Multi-tenant error types.

use thiserror::Error;

use crate::error::AppError;

/// Errors that can occur during tenant resolution or tenant-scoped operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TenantError {
    /// No tenant could be resolved from the request.
    #[error("missing tenant identifier")]
    MissingTenant,

    /// The specified tenant was not found or is inactive.
    #[error("tenant not found: '{0}'")]
    TenantNotFound(String),

    /// The tenant resolver encountered an internal error.
    #[error("tenant resolver error: {0}")]
    ResolverError(String),

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
            TenantError::ResolverError(msg) => AppError::BadRequest {
                code: "TENANT_RESOLVER_ERROR",
                message: msg,
            },
            TenantError::Conflict(msg) => AppError::Public {
                status: axum::http::StatusCode::CONFLICT,
                code: "TENANT_CONFLICT",
                message: msg,
            },
            TenantError::QuotaExceeded { tenant_id, limit } => AppError::Public {
                status: axum::http::StatusCode::TOO_MANY_REQUESTS,
                code: "TENANT_QUOTA_EXCEEDED",
                message: format!("Tenant '{tenant_id}' has exceeded its tiered quota limit of {limit}"),
            },
        }
    }
}
