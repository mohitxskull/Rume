//! Application error type and HTTP response mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

/// A single validation error detail, with the field path and message.
#[derive(Debug, Serialize)]
pub struct ValidationErrorDetail {
    pub field: String,
    pub message: String,
}

/// Errors that can be returned from a Rume handler.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("validation failed")]
    Validation { details: Vec<ValidationErrorDetail> },

    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();

        match self {
            Self::Validation { details } => {
                let body = Json(serde_json::json!({
                    "error": "validation failed",
                    "details": details,
                }));
                (status, body).into_response()
            }
            _ => {
                let body = Json(ErrorBody {
                    error: self.to_string(),
                });
                (status, body).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_codes() {
        assert_eq!(AppError::NotFound.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(
            AppError::Validation { details: vec![] }.status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            AppError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
