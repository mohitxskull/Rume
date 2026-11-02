//! Request validation helpers.

use axum::Json;
use axum::extract::FromRequest;
use axum::http::Request;
use garde::Validate;

use crate::error::{AppError, ValidationErrorDetail};

/// An extractor that validates its payload before handing it to a handler.
///
/// Use it in place of `Json<T>` when you want declarative field validation via
/// [`garde::Validate`]:
///
/// ```ignore
/// use rume::validation::Validated;
/// use rume::prelude::*;
/// use garde::Validate;
///
/// #[derive(Deserialize, Validate)]
/// #[serde(rename_all = "camelCase")]
/// struct CreateUser {
///     #[garde(length(min = 3, max = 50))]
///     username: String,
///     #[garde(email)]
///     email: String,
///     #[garde(range(min = 18, max = 120))]
///     age: u32,
/// }
///
/// pub async fn create_user(Validated(payload): Validated<CreateUser>) -> Json<...> {
///     // payload is guaranteed valid
/// }
/// ```
pub struct Validated<T>(pub T);

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
        let json =
            Json::<T>::from_request(req, state)
                .await
                .map_err(|err| AppError::BadRequest {
                    code: "BAD_REQUEST",
                    message: err.to_string(),
                })?;

        json.0.validate().map_err(|report| {
            let details: Vec<ValidationErrorDetail> = report
                .iter()
                .map(|(path, error)| ValidationErrorDetail {
                    field: path.to_string(),
                    message: error.message().to_string(),
                })
                .collect();
            AppError::Validation { details }
        })?;

        Ok(Validated(json.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestApp;
    use axum::Json;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::post;
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

    async fn create_user(Validated(_payload): Validated<CreateUser>) -> Json<serde_json::Value> {
        Json(serde_json::json!({ "status": "ok" }))
    }

    async fn build_app() -> TestApp {
        let router = Router::new().route("/users", post(create_user));
        TestApp::new(router)
    }

    #[tokio::test]
    async fn valid_payload_passes_validation() {
        let app = build_app().await;
        let body = r#"{"username":"alice","email":"alice@example.com","age":30}"#;

        let response = app.post_json("/users", body).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_payload_returns_422_with_details() {
        let app = build_app().await;
        let body = r#"{"username":"ab","email":"not-an-email","age":200}"#;

        let response = app.post_json("/users", body).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let body_text = TestApp::body_text(response).await;
        let value: serde_json::Value = serde_json::from_str(&body_text).unwrap();

        assert_eq!(value["code"], "VALIDATION_FAILED");
        let details = value["details"].as_array().unwrap();
        assert!(!details.is_empty());

        // Check each expected field is present with a message.
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
        let app = build_app().await;
        let body = r#"{"email":"alice@example.com"}"#;

        let response = app.post_json("/users", body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
