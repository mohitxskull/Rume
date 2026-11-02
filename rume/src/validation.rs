//! Request validation helpers.
//!
//! Provides typed request extractors that automatically validate their payloads using [`garde::Validate`].
//! All extractors produce unified, sanitized RFC 9457 Problem Details rejection responses.

use std::ops::{Deref, DerefMut};

use axum::extract::rejection::{FormRejection, JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use garde::Validate;

use crate::error::{AppError, ValidationErrorDetail};

fn map_validation_report(report: garde::Report) -> AppError {
    let details: Vec<ValidationErrorDetail> = report
        .iter()
        .map(|(path, error)| ValidationErrorDetail {
            field: path.to_string(),
            message: error.message().to_string(),
        })
        .collect();
    AppError::Validation { details }
}

macro_rules! impl_wrapper_traits {
    ($name:ident) => {
        impl<T> $name<T> {
            /// Consumes the wrapper and returns the inner value.
            #[inline]
            pub fn into_inner(self) -> T {
                self.0
            }
        }

        impl<T> Deref for $name<T> {
            type Target = T;

            #[inline]
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<T> DerefMut for $name<T> {
            #[inline]
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl<T> AsRef<T> for $name<T> {
            #[inline]
            fn as_ref(&self) -> &T {
                &self.0
            }
        }

        impl<T> From<T> for $name<T> {
            #[inline]
            fn from(value: T) -> Self {
                Self(value)
            }
        }
    };
}

/// An extractor that validates a JSON request body before handing it to a handler.
///
/// If parsing fails, it rejects with [`AppError::BadRequest`].
/// If validation fails, it rejects with [`AppError::Validation`] (RFC 9457 422 Problem Details).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatedJson<T>(pub T);

impl_wrapper_traits!(ValidatedJson);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: serde::de::DeserializeOwned + Validate<Context = ()>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let json =
            axum::Json::<T>::from_request(req, state)
                .await
                .map_err(|err: JsonRejection| AppError::BadRequest {
                    code: "BAD_REQUEST",
                    message: err.to_string(),
                })?;

        json.0.validate().map_err(map_validation_report)?;

        Ok(ValidatedJson(json.0))
    }
}

/// An extractor that validates a JSON request body before handing it to a handler.
///
/// Equivalent to [`ValidatedJson`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Validated<T>(pub T);

impl_wrapper_traits!(Validated);

impl<T, S> FromRequest<S> for Validated<T>
where
    T: serde::de::DeserializeOwned + Validate<Context = ()>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let validated = ValidatedJson::<T>::from_request(req, state).await?;
        Ok(Validated(validated.0))
    }
}

/// An extractor that validates URL query parameters before handing them to a handler.
///
/// Implements [`FromRequestParts`], meaning it does not consume the request body and can be combined
/// with other body extractors in any order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatedQuery<T>(pub T);

impl_wrapper_traits!(ValidatedQuery);

impl<T, S> FromRequestParts<S> for ValidatedQuery<T>
where
    T: serde::de::DeserializeOwned + Validate<Context = ()>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let query = axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|err: QueryRejection| AppError::BadRequest {
                code: "BAD_REQUEST",
                message: err.to_string(),
            })?;

        query.0.validate().map_err(map_validation_report)?;

        Ok(ValidatedQuery(query.0))
    }
}

/// An extractor that validates URL path parameters before handing them to a handler.
///
/// Implements [`FromRequestParts`], meaning it does not consume the request body and can be combined
/// with other body extractors in any order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatedPath<T>(pub T);

impl_wrapper_traits!(ValidatedPath);

impl<T, S> FromRequestParts<S> for ValidatedPath<T>
where
    T: serde::de::DeserializeOwned + Validate<Context = ()> + Send + 'static,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let path = axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(|err: PathRejection| AppError::BadRequest {
                code: "BAD_REQUEST",
                message: err.to_string(),
            })?;

        path.0.validate().map_err(map_validation_report)?;

        Ok(ValidatedPath(path.0))
    }
}

/// An extractor that validates URL-encoded form request bodies before handing them to a handler.
///
/// If parsing fails, it rejects with [`AppError::BadRequest`].
/// If validation fails, it rejects with [`AppError::Validation`] (RFC 9457 422 Problem Details).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatedForm<T>(pub T);

impl_wrapper_traits!(ValidatedForm);

impl<T, S> FromRequest<S> for ValidatedForm<T>
where
    T: serde::de::DeserializeOwned + Validate<Context = ()>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let form = axum::extract::Form::<T>::from_request(req, state)
            .await
            .map_err(|err: FormRejection| AppError::BadRequest {
                code: "BAD_REQUEST",
                message: err.to_string(),
            })?;

        form.0.validate().map_err(map_validation_report)?;

        Ok(ValidatedForm(form.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestApp;
    use axum::Json;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use garde::Validate;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Validate)]
    struct CreateUser {
        #[garde(length(min = 3, max = 50))]
        username: String,
        #[garde(email)]
        email: String,
        #[garde(range(min = 18, max = 120))]
        age: u32,
    }

    #[derive(Debug, Deserialize, Validate)]
    struct UserQuery {
        #[garde(length(min = 2, max = 30))]
        search: String,
        #[garde(range(min = 1, max = 100))]
        page: u32,
    }

    #[derive(Debug, Deserialize, Validate)]
    struct UserPath {
        #[garde(length(min = 3, max = 20))]
        username: String,
    }

    #[derive(Debug, Deserialize, Validate)]
    struct UserForm {
        #[garde(email)]
        email: String,
        #[garde(length(min = 8))]
        password: String,
    }

    async fn create_user_json(
        Validated(_payload): Validated<CreateUser>,
    ) -> Json<serde_json::Value> {
        Json(serde_json::json!({ "status": "ok" }))
    }

    async fn get_users_query(
        ValidatedQuery(query): ValidatedQuery<UserQuery>,
    ) -> Json<serde_json::Value> {
        Json(serde_json::json!({ "search": query.search, "page": query.page }))
    }

    async fn get_user_by_path(
        ValidatedPath(path): ValidatedPath<UserPath>,
    ) -> Json<serde_json::Value> {
        Json(serde_json::json!({ "username": path.username }))
    }

    async fn submit_form(ValidatedForm(form): ValidatedForm<UserForm>) -> Json<serde_json::Value> {
        Json(serde_json::json!({ "email": form.email }))
    }

    fn build_app() -> TestApp {
        let router = Router::new()
            .route("/users", post(create_user_json))
            .route("/users", get(get_users_query))
            .route("/users/{username}", get(get_user_by_path))
            .route("/form", post(submit_form));
        TestApp::new(router)
    }

    #[tokio::test]
    async fn valid_payload_passes_validation() {
        let app = build_app();
        let body = r#"{"username":"alice","email":"alice@example.com","age":30}"#;

        let response = app.post_json("/users", body).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_payload_returns_422_with_details() {
        let app = build_app();
        let body = r#"{"username":"ab","email":"not-an-email","age":200}"#;

        let response = app.post_json("/users", body).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let body_text = TestApp::body_text(response).await;
        let value: serde_json::Value = serde_json::from_str(&body_text).unwrap();

        assert_eq!(value["code"], "VALIDATION_FAILED");
        let details = value["details"].as_array().unwrap();
        assert!(!details.is_empty());

        let fields: Vec<&str> = details
            .iter()
            .map(|d| d["field"].as_str().unwrap())
            .collect();
        assert!(fields.contains(&"username"));
        assert!(fields.contains(&"email"));
        assert!(fields.contains(&"age"));

        for detail in details {
            assert!(!detail["message"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn missing_required_field_returns_400() {
        let app = build_app();
        let body = r#"{"email":"alice@example.com"}"#;

        let response = app.post_json("/users", body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn query_extractor_validation() {
        let app = build_app();

        // Valid query
        let res = app.get("/users?search=rust&page=2").await;
        assert_eq!(res.status(), StatusCode::OK);

        // Validation error in query -> 422
        let res = app.get("/users?search=r&page=200").await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body_text = TestApp::body_text(res).await;
        let val: serde_json::Value = serde_json::from_str(&body_text).unwrap();
        assert_eq!(val["code"], "VALIDATION_FAILED");

        // Parse error in query -> 400
        let res = app.get("/users?search=rust&page=invalid").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn path_extractor_validation() {
        let app = build_app();

        // Valid path
        let res = app.get("/users/alice").await;
        assert_eq!(res.status(), StatusCode::OK);

        // Validation error in path -> 422 (too short)
        let res = app.get("/users/al").await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn form_extractor_validation() {
        let app = build_app();

        // Valid form
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/form")
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(axum::body::Body::from(
                "email=alice%40example.com&password=secret12345",
            ))
            .unwrap();
        let res = app.request(req).await;
        assert_eq!(res.status(), StatusCode::OK);

        // Invalid form validation -> 422
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/form")
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(axum::body::Body::from("email=not-an-email&password=short"))
            .unwrap();
        let res = app.request(req).await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Malformed form data -> 400
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/form")
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(axum::body::Body::from("invalid_field=only"))
            .unwrap();
        let res = app.request(req).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
