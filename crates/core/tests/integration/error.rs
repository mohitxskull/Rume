//! Integration tests for the unified HTTP error contract.
//!
//! Verifies that all `AppError` variants produce RFC 9457 Problem Details
//! responses with the `application/problem+json` content type, and that
//! extractor rejections unify into the same error model.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use rume::error::AppError;
use rume::routing::{Matcher, ValidatedParam};
use rume::testing::TestApp;
use serde_json::Value;
use std::collections::BTreeMap;

async fn handler_bad_request() -> Result<(), AppError> {
    Err(AppError::BadRequest {
        code: "INVALID_INPUT",
        message: "something went wrong".to_string(),
    })
}

async fn handler_not_found() -> Result<(), AppError> {
    Err(AppError::NotFound)
}

async fn handler_validation() -> Result<(), AppError> {
    Err(AppError::Validation {
        details: vec![rume::error::ValidationErrorDetail {
            field: "email".to_string(),
            message: "invalid format".to_string(),
        }],
    })
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = TestApp::body_bytes(response).await;
    let text = String::from_utf8(bytes).expect("body is utf-8");
    serde_json::from_str(&text).expect("body is valid JSON")
}

fn content_type(response: &axum::response::Response) -> Option<String> {
    response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

#[tokio::test]
async fn bad_request_returns_problem_details() {
    let app =
        TestApp::new(axum::Router::new().route("/bad", axum::routing::get(handler_bad_request)));

    let response = app.get("/bad").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        content_type(&response),
        Some("application/problem+json".to_string())
    );

    let body = body_json(response).await;
    insta::assert_json_snapshot!(body, @r###"
    {
      "code": "INVALID_INPUT",
      "detail": "The request could not be processed.",
      "instance": null,
      "request_id": null,
      "status": 400,
      "title": "Bad Request",
      "type": "about:blank"
    }
    "###);
    // Raw message must not leak to clients
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.contains("something went wrong"));
}

#[tokio::test]
async fn not_found_returns_problem_details() {
    let app =
        TestApp::new(axum::Router::new().route("/missing", axum::routing::get(handler_not_found)));

    let response = app.get("/missing").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        content_type(&response),
        Some("application/problem+json".to_string())
    );

    let body = body_json(response).await;
    insta::assert_json_snapshot!(body, @r###"
    {
      "code": "NOT_FOUND",
      "detail": "The requested resource was not found.",
      "instance": null,
      "request_id": null,
      "status": 404,
      "title": "Not Found",
      "type": "about:blank"
    }
    "###);
}

#[tokio::test]
async fn validation_returns_problem_details_with_details() {
    let app = TestApp::new(
        axum::Router::new().route("/validate", axum::routing::get(handler_validation)),
    );

    let response = app.get("/validate").await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        content_type(&response),
        Some("application/problem+json".to_string())
    );

    let body = body_json(response).await;
    assert_eq!(body["code"].as_str(), Some("VALIDATION_FAILED"));
    assert_eq!(body["status"].as_u64(), Some(422));
    let details = body["details"].as_array().expect("details array present");
    assert_eq!(details.len(), 1);
    assert_eq!(details[0]["field"].as_str(), Some("email"));
}

#[tokio::test]
async fn validated_param_number_succeeds() {
    // /users/42 → OK
    let app = TestApp::new(axum::Router::new().route(
        "/users/{id}",
        axum::routing::get(
            move |params: axum::extract::Path<BTreeMap<String, String>>| async move {
                let id =
                    ValidatedParam::from_value("id", params.get("id").unwrap(), Matcher::Number)
                        .unwrap();
                format!("user {}", id.value)
            },
        ),
    ));

    let response = app.get("/users/42").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert_eq!(body, "user 42");
}

#[tokio::test]
async fn validated_param_number_rejects() {
    // /users/abc → 400 with Problem Details
    let app = TestApp::new(axum::Router::new().route(
        "/users/{id}",
        axum::routing::get(
            move |params: axum::extract::Path<BTreeMap<String, String>>| async move {
                let id = match ValidatedParam::from_value(
                    "id",
                    params.get("id").unwrap(),
                    Matcher::Number,
                ) {
                    Ok(id) => id,
                    Err(err) => return err.into_response(),
                };
                format!("user {}", id.value).into_response()
            },
        ),
    ));

    let response = app.get("/users/abc").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        content_type(&response),
        Some("application/problem+json".to_string())
    );

    let body = body_json(response).await;
    assert_eq!(body["code"].as_str(), Some("INVALID_PARAM"));
    // Raw input must not be echoed
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.contains("abc"));
}
