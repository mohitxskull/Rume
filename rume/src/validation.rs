//! Request validation helpers.

use axum::Json;
use axum::extract::FromRequest;
use axum::http::Request;
use garde::Validate;

use crate::error::AppError;

/// An extractor that validates its payload before handing it to a handler.
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
        let json = Json::<T>::from_request(req, state)
            .await
            .map_err(|err| AppError::BadRequest(err.to_string()))?;

        json.0
            .validate()
            .map_err(|err| AppError::Validation(err.to_string()))?;

        Ok(Validated(json.0))
    }
}
