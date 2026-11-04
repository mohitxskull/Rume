//! Application error type and HTTP response mapping.
//!
//! Errors are split into two concerns:
//! - client-visible, developer-intentional errors ([`AppError::Public`] and
//!   the safe generic variants), rendered to the client as-is;
//! - internal diagnostic errors ([`AppError::Internal`]), whose source is
//!   logged server-side while clients receive a generic message.
//!
//! All responses use the RFC 9457 Problem Details format
//! (<https://www.rfc-editor.org/rfc/rfc9457>).

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;
use tracing::{error, warn};

/// The RFC 9457 problem type URI.
const PROBLEM_TYPE_URI: &str = "about:blank";

/// The RFC 9457 media type for Problem Details responses.
const PROBLEM_JSON_MEDIA_TYPE: &str = "application/problem+json";

/// Build an RFC 9457 Problem Details response with the correct media type.
fn problem_details_response(status: StatusCode, details: &ProblemDetails) -> Response {
    let body = serde_json::to_string(details).unwrap_or_else(|_| {
        serde_json::json!({
            "type": PROBLEM_TYPE_URI,
            "title": "Error",
            "status": status.as_u16(),
            "detail": "An error occurred.",
            "code": "ERROR",
        })
        .to_string()
    });

    let builder = Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, PROBLEM_JSON_MEDIA_TYPE);
    builder.body(Body::from(body)).unwrap()
}

/// A single validation error detail, with the field path and message.
#[derive(Debug, Serialize)]
pub struct ValidationErrorDetail {
    pub field: String,
    pub message: String,
}

/// RFC 9457 Problem Details response body.
///
/// `details` is a Rume extension carrying structured validation errors; it
/// is omitted entirely when absent. `instance` and `request_id` serialize
/// as `null` when absent, matching the RFC's optional treatment.
#[derive(Serialize)]
struct ProblemDetails {
    r#type: String,
    title: String,
    status: u16,
    detail: String,
    instance: Option<String>,
    code: String,
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Vec<ValidationErrorDetail>>,
}

/// The diagnostic source of an [`AppError::Internal`], kept as a string.
///
/// Wrapped in a newtype so thiserror treats it as the chained error
/// source (`String` itself does not implement `std::error::Error`). The
/// source is logged server-side via tracing rather than surfaced to
/// clients.
#[derive(Debug)]
pub struct InternalSource(pub(crate) String);

impl InternalSource {
    /// Creates a new internal diagnostic error source.
    pub fn new(source: impl Into<String>) -> Self {
        Self(source.into())
    }
}

impl From<String> for InternalSource {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for InternalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for InternalSource {}

/// Errors that can be returned from a Rume handler.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("bad request: {message}")]
    BadRequest { code: &'static str, message: String },

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("validation failed")]
    Validation { details: Vec<ValidationErrorDetail> },

    #[error("internal server error")]
    Internal {
        source: InternalSource,
        request_id: Option<String>,
    },

    #[error("{message}")]
    Public {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

impl AppError {
    /// Creates an internal server error from a diagnostic message.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal {
            source: InternalSource(msg.into()),
            request_id: None,
        }
    }

    /// Creates a 400 Bad Request error with a custom message.
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest {
            code: "BAD_REQUEST",
            message: msg.into(),
        }
    }

    /// Creates a 404 Not Found error with a custom message.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::Public {
            status: StatusCode::NOT_FOUND,
            code: "NOT_FOUND",
            message: msg.into(),
        }
    }

    /// The HTTP status code for this error.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest { .. } => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Public { status, .. } => *status,
        }
    }
}

/// A Problem Details body with the standard fields filled in.
///
/// `title` and `detail` are static because they describe the *class* of
/// error, not the triggering request; the raw request-specific detail is
/// never echoed here — that is the job of [`AppError::Public`] (which
/// passes a developer-intentional message) or the server-side logs.
fn problem(
    status: StatusCode,
    title: &'static str,
    detail: &'static str,
    code: &'static str,
    request_id: Option<String>,
    details: Option<Vec<ValidationErrorDetail>>,
) -> ProblemDetails {
    ProblemDetails {
        r#type: "about:blank".to_string(),
        title: title.to_string(),
        status: status.as_u16(),
        detail: detail.to_string(),
        instance: None,
        code: code.to_string(),
        request_id,
        details,
    }
}

/// Human-readable title for a status code, used by [`AppError::Public`].
///
/// 422 renders as "Validation Failed" rather than the canonical
/// "Unprocessable Entity", since validation failures are reported to the
/// client in that form. Non-standard codes without a canonical reason fall
/// back to "Error".
fn title_for(status: StatusCode) -> &'static str {
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        return "Validation Failed";
    }
    status.canonical_reason().unwrap_or("Error")
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();

        match self {
            Self::NotFound => problem_details_response(
                status,
                &problem(
                    status,
                    "Not Found",
                    "The requested resource was not found.",
                    "NOT_FOUND",
                    None,
                    None,
                ),
            ),
            Self::BadRequest { code, message } => {
                // S-M4: log the detail server-side; clients get a generic
                // message. The raw message must never be echoed to clients.
                warn!("bad request: {}", message);
                problem_details_response(
                    status,
                    &problem(
                        status,
                        "Bad Request",
                        "The request could not be processed.",
                        code,
                        None,
                        None,
                    ),
                )
            }
            Self::Unauthorized => problem_details_response(
                status,
                &problem(
                    status,
                    "Unauthorized",
                    "The request requires authentication.",
                    "UNAUTHORIZED",
                    None,
                    None,
                ),
            ),
            Self::Forbidden => problem_details_response(
                status,
                &problem(
                    status,
                    "Forbidden",
                    "The request is forbidden.",
                    "FORBIDDEN",
                    None,
                    None,
                ),
            ),
            Self::Validation { details } => problem_details_response(
                status,
                &problem(
                    status,
                    "Validation Failed",
                    "One or more fields failed validation.",
                    "VALIDATION_FAILED",
                    None,
                    Some(details),
                ),
            ),
            Self::Internal { source, request_id } => {
                // S-M4: log the source server-side; clients get a generic
                // message. The request_id is echoed so the client can
                // reference it when contacting support.
                error!(%source, "internal server error");
                problem_details_response(
                    status,
                    &problem(
                        status,
                        "Internal Server Error",
                        "An unexpected error occurred.",
                        "INTERNAL_ERROR",
                        request_id,
                        None,
                    ),
                )
            }
            Self::Public {
                status: custom_status,
                code,
                message,
            } => {
                // Developer-intentional client error: render as-is. Not
                // logged — it is intentional, not an internal failure.
                problem_details_response(
                    custom_status,
                    &ProblemDetails {
                        r#type: PROBLEM_TYPE_URI.to_string(),
                        title: title_for(custom_status).to_string(),
                        status: custom_status.as_u16(),
                        detail: message,
                        instance: None,
                        code: code.to_string(),
                        request_id: None,
                        details: None,
                    },
                )
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
            AppError::BadRequest {
                code: "BAD_REQUEST",
                message: "x".into(),
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Unauthorized.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(AppError::Forbidden.status_code(), StatusCode::FORBIDDEN);
        assert_eq!(
            AppError::Validation { details: vec![] }.status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            AppError::Internal {
                source: InternalSource("x".into()),
                request_id: None,
            }
            .status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            AppError::Public {
                status: StatusCode::IM_A_TEAPOT,
                code: "TEAPOT",
                message: "x".into(),
            }
            .status_code(),
            StatusCode::IM_A_TEAPOT
        );
    }

    /// Extract the JSON body string from an IntoResponse.
    async fn body_of<T: IntoResponse>(resp: T) -> String {
        let response = resp.into_response();
        let (_parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Extract and parse the JSON body from an IntoResponse.
    async fn body_json<T: IntoResponse>(resp: T) -> serde_json::Value {
        serde_json::from_str(&body_of(resp).await).expect("error body is valid JSON")
    }

    #[tokio::test]
    async fn not_found_body_is_safe() {
        // S-M4: NotFound renders a safe generic Problem Details body.
        let value = body_json(AppError::NotFound).await;
        assert_eq!(value["type"].as_str(), Some("about:blank"));
        assert_eq!(value["title"].as_str(), Some("Not Found"));
        assert_eq!(value["status"].as_u64(), Some(404));
        assert_eq!(
            value["detail"].as_str(),
            Some("The requested resource was not found.")
        );
        assert_eq!(value["code"].as_str(), Some("NOT_FOUND"));
        assert!(value["instance"].is_null());
        assert!(value["request_id"].is_null());
    }

    #[tokio::test]
    async fn unauthorized_body_is_safe() {
        let value = body_json(AppError::Unauthorized).await;
        assert_eq!(value["status"].as_u64(), Some(401));
        assert_eq!(value["code"].as_str(), Some("UNAUTHORIZED"));
        assert_eq!(value["title"].as_str(), Some("Unauthorized"));
    }

    #[tokio::test]
    async fn forbidden_body_is_safe() {
        let value = body_json(AppError::Forbidden).await;
        assert_eq!(value["status"].as_u64(), Some(403));
        assert_eq!(value["code"].as_str(), Some("FORBIDDEN"));
        assert_eq!(value["title"].as_str(), Some("Forbidden"));
    }

    #[tokio::test]
    async fn bad_request_generic_message_to_client() {
        // S-M4: BadRequest detail must NOT be echoed to clients.
        let body = body_of(AppError::BadRequest {
            code: "BAD_REQUEST",
            message: "leaky detail: password=xyz".into(),
        })
        .await;
        let value = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        assert_eq!(value["status"].as_u64(), Some(400));
        assert_eq!(value["code"].as_str(), Some("BAD_REQUEST"));
        assert_eq!(
            value["detail"].as_str(),
            Some("The request could not be processed.")
        );
        assert!(!body.contains("password=xyz"));
        assert!(!body.contains("leaky detail"));
    }

    #[tokio::test]
    async fn internal_generic_message_to_client() {
        // S-M4: Internal source must NOT be echoed to clients; request_id
        // is echoed so the client can reference it in support.
        let request_id = "req-123".to_string();
        let body = body_of(AppError::Internal {
            source: InternalSource("connection string: postgresql://user:pass@db".into()),
            request_id: Some(request_id.clone()),
        })
        .await;
        let value = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        assert_eq!(value["status"].as_u64(), Some(500));
        assert_eq!(value["code"].as_str(), Some("INTERNAL_ERROR"));
        assert_eq!(
            value["detail"].as_str(),
            Some("An unexpected error occurred.")
        );
        assert_eq!(value["request_id"].as_str(), Some("req-123"));
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
        let value = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        assert_eq!(value["status"].as_u64(), Some(422));
        assert_eq!(value["code"].as_str(), Some("VALIDATION_FAILED"));
        assert_eq!(value["title"].as_str(), Some("Validation Failed"));
        let details = value["details"].as_array().expect("details array present");
        assert_eq!(details.len(), 1);
        assert_eq!(details[0]["field"].as_str(), Some("email"));
        assert_eq!(details[0]["message"].as_str(), Some("invalid format"));
    }

    #[tokio::test]
    async fn public_error_uses_custom_status_and_message() {
        // Public errors are developer-intentional: custom status and message
        // are rendered to the client as-is, and nothing is logged server-side.
        let body = body_of(AppError::Public {
            status: StatusCode::IM_A_TEAPOT,
            code: "TEAPOT_ERROR",
            message: "I am a teapot.".into(),
        })
        .await;
        let value = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        assert_eq!(value["status"].as_u64(), Some(418));
        assert_eq!(value["code"].as_str(), Some("TEAPOT_ERROR"));
        assert_eq!(value["detail"].as_str(), Some("I am a teapot."));
    }

    #[tokio::test]
    async fn problem_details_has_standard_fields() {
        // Every Problem Details body carries the standard RFC 9457 fields.
        let value = body_json(AppError::NotFound).await;
        for field in [
            "type",
            "title",
            "status",
            "detail",
            "instance",
            "code",
            "request_id",
        ] {
            assert!(
                value.get(field).is_some(),
                "Problem Details body missing field {field}"
            );
        }
        assert_eq!(value["type"].as_str(), Some("about:blank"));
        assert!(value["instance"].is_null());
        assert!(value["request_id"].is_null());
    }
}
