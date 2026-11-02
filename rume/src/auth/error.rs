//! Authentication and authorization error types.

use thiserror::Error;

use crate::error::AppError;

/// Errors that can occur during authentication or authorization.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    /// No credentials were provided in the request.
    #[error("missing credentials")]
    MissingCredentials,

    /// Credentials provided are invalid or malformed.
    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),

    /// The authentication token or session has expired.
    #[error("credentials expired")]
    Expired,

    /// The authenticated identity does not have sufficient permissions.
    #[error("forbidden: required permission or role not satisfied")]
    Forbidden,

    /// An internal error occurred during authentication.
    #[error("authentication provider error: {0}")]
    ProviderError(String),
}

impl From<AuthError> for AppError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Forbidden => AppError::Forbidden,
            AuthError::MissingCredentials
            | AuthError::InvalidCredentials(_)
            | AuthError::Expired => AppError::Unauthorized,
            AuthError::ProviderError(msg) => AppError::BadRequest {
                code: "AUTH_ERROR",
                message: msg,
            },
        }
    }
}
