//! Extractor rejections.
//!
//! Provides [`Rejection`] for [`crate::routing::ValidatedParam`] failures,
//! which converts into [`crate::error::AppError`] so extractors, validation,
//! and application handlers follow one consistent error model.

use axum::response::{IntoResponse, Response};

use crate::error::AppError;

/// Rejection reasons for `ValidatedParam`.
#[derive(Debug)]
pub enum Rejection {
    Extraction(String),
    MissingParam(String),
    Validation(String),
}

impl From<Rejection> for AppError {
    fn from(rejection: Rejection) -> Self {
        match rejection {
            Rejection::Extraction(err) => AppError::BadRequest {
                code: "EXTRACTION_FAILED",
                message: err,
            },
            Rejection::MissingParam(name) => AppError::BadRequest {
                code: "MISSING_PARAM",
                message: format!("missing parameter: {name}"),
            },
            Rejection::Validation(err) => AppError::BadRequest {
                code: "VALIDATION_FAILED",
                message: err,
            },
        }
    }
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        AppError::from(self).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use axum::http::StatusCode;

    #[test]
    fn rejection_into_app_error() {
        // All rejection variants convert to AppError::BadRequest (400).
        let extraction: AppError = Rejection::Extraction("bad".to_string()).into();
        assert_eq!(extraction.status_code(), StatusCode::BAD_REQUEST);

        let missing: AppError = Rejection::MissingParam("id".to_string()).into();
        assert_eq!(missing.status_code(), StatusCode::BAD_REQUEST);

        let validation: AppError = Rejection::Validation("bad value".to_string()).into();
        assert_eq!(validation.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejection_into_response_uses_problem_details() {
        // Rejection::IntoResponse delegates to AppError, producing Problem Details.
        let response = Rejection::MissingParam("id".to_string()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        assert_eq!(content_type, Some("application/problem+json"));
    }
}
