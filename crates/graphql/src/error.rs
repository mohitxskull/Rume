//! `AppError` to `async_graphql::Error` mapping with structured extensions.

use async_graphql::ErrorExtensions as _;
use rume_foundation::error::AppError;

/// Maps a [`rume_foundation::error::AppError`] to an [`async_graphql::Error`] with
/// structured `code` and `status` extensions.
pub fn app_error_to_graphql(err: &AppError) -> async_graphql::Error {
    let status = err.status_code().as_u16();
    let (message, code): (String, &'static str) = match err {
        AppError::NotFound => ("not found".to_string(), "NOT_FOUND"),
        AppError::BadRequest { code, message } => (message.clone(), code),
        AppError::Unauthorized => ("unauthorized".to_string(), "UNAUTHORIZED"),
        AppError::Forbidden => ("forbidden".to_string(), "FORBIDDEN"),
        AppError::Validation { .. } => ("validation failed".to_string(), "VALIDATION_FAILED"),
        AppError::Conflict { code, message } => (message.clone(), code),
        // Why: never expose internal diagnostic `source.message` to GraphQL callers.
        AppError::Internal { .. } => ("internal server error".to_string(), "INTERNAL_ERROR"),
        AppError::Public { code, message, .. } => (message.clone(), code),
    };

    async_graphql::Error::new(message).extend_with(|_, e| {
        e.set("code", code);
        e.set("status", status);
    })
}
