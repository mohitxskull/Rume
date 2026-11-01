//! Application error type and HTTP response mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;
use tracing::{error, warn};

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

        match &self {
            Self::Validation { details } => {
                let body = Json(serde_json::json!({
                    "error": "validation failed",
                    "details": details,
                }));
                (status, body).into_response()
            }
            Self::Internal(detail) => {
                // S-M4: never echo internal detail to clients; log server-side
                // instead. Clients receive a generic message.
                error!(error = %detail, "internal server error");
                let body = Json(ErrorBody {
                    error: "internal server error".to_string(),
                });
                (status, body).into_response()
            }
            Self::BadRequest(detail) => {
                // S-M4: generic message to clients; detail logged server-side.
                warn!(error = %detail, "bad request");
                let body = Json(ErrorBody {
                    error: "bad request".to_string(),
                });
                (status, body).into_response()
            }
            _ => {
                // NotFound, Unauthorized, Forbidden all render safe, generic
                // messages via their Display impls.
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
    use axum::body::to_bytes;
    use tokio;

    #[tokio::test]
    async fn maps_status_codes() {
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

    /// Extract the JSON body string from an IntoResponse.
    async fn body_of<T: IntoResponse>(resp: T) -> String {
        let response = resp.into_response();
        let (_parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn not_found_body_is_safe() {
        // S-M4: NotFound renders a safe generic message.
        let body = body_of(AppError::NotFound).await;
        assert!(body.contains("\"error\":\"not found\""));
    }

    #[tokio::test]
    async fn unauthorized_body_is_safe() {
        let body = body_of(AppError::Unauthorized).await;
        assert!(body.contains("\"error\":\"unauthorized\""));
    }

    #[tokio::test]
    async fn forbidden_body_is_safe() {
        let body = body_of(AppError::Forbidden).await;
        assert!(body.contains("\"error\":\"forbidden\""));
    }

    #[tokio::test]
    async fn bad_request_generic_message_to_client() {
        // S-M4: BadRequest detail must NOT be echoed to clients.
        let body = body_of(AppError::BadRequest("leaky detail: password=xyz".into())).await;
        assert!(body.contains("\"error\":\"bad request\""));
        assert!(!body.contains("password=xyz"));
        assert!(!body.contains("leaky detail"));
    }

    #[tokio::test]
    async fn internal_generic_message_to_client() {
        // S-M4: Internal detail must NOT be echoed to clients.
        let body = body_of(AppError::Internal(
            "connection string: postgresql://user:pass@db".into(),
        ))
        .await;
        assert!(body.contains("\"error\":\"internal server error\""));
        assert!(!body.contains("postgresql://"));
        assert!(!body.contains("connection string"));
    }

    #[tokio::test]
    async fn validation_body_has_details() {
        let body = body_of(AppError::Validation {
            details: vec![ValidationErrorDetail {
                field: "email".to_string(),
                message: "invalid format".to_string(),
            }],
        })
        .await;
        assert!(body.contains("\"error\":\"validation failed\""));
        assert!(body.contains("\"field\":\"email\""));
        assert!(body.contains("\"message\":\"invalid format\""));
    }
}
