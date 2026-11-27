//! `AppError` to `tonic::Status` mapping.

use axum::http::StatusCode;
use rume_foundation::error::AppError;
use tonic::Status;

/// Maps a [`rume_foundation::error::AppError`] to a canonical [`tonic::Status`].
pub fn app_error_to_status(err: &AppError) -> Status {
    match err {
        AppError::NotFound => Status::not_found("not found"),
        AppError::NotFoundWithDetail { message } => Status::not_found(message),
        AppError::BadRequest { message, .. } => Status::invalid_argument(message),
        AppError::Unauthorized => Status::unauthenticated("unauthorized"),
        AppError::Forbidden => Status::permission_denied("forbidden"),
        AppError::Validation { details } => {
            if details.is_empty() {
                Status::invalid_argument("validation failed")
            } else {
                let summary = details
                    .iter()
                    .map(|d| format!("{}: {}", d.field, d.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                Status::invalid_argument(summary)
            }
        }
        AppError::Conflict { message, .. } => Status::already_exists(message),
        // Why: never expose internal diagnostic `source.message` to gRPC callers.
        AppError::Internal { .. } => Status::internal("internal server error"),
        AppError::Public {
            status, message, ..
        } => match *status {
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                Status::invalid_argument(message)
            }
            StatusCode::UNAUTHORIZED => Status::unauthenticated(message),
            StatusCode::FORBIDDEN => Status::permission_denied(message),
            StatusCode::NOT_FOUND => Status::not_found(message),
            StatusCode::CONFLICT => Status::already_exists(message),
            StatusCode::PRECONDITION_FAILED => Status::failed_precondition(message),
            StatusCode::TOO_MANY_REQUESTS => Status::resource_exhausted(message),
            StatusCode::NOT_IMPLEMENTED => Status::unimplemented(message),
            StatusCode::SERVICE_UNAVAILABLE => Status::unavailable(message),
            StatusCode::GATEWAY_TIMEOUT => Status::deadline_exceeded(message),
            s if s.is_client_error() => Status::invalid_argument(message),
            _ => Status::internal(message),
        },
    }
}
