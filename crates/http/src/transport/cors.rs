//! Fail-closed CORS configuration and layer builder backed by `tower-http`.

use std::time::Duration;

/// Tower layer enforcing configured CORS rules.
pub type CorsLayer = tower_http::cors::CorsLayer;

/// Error returned when a [`CorsConfig`] violates Fetch spec or `tower-http` invariants.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorsConfigError {
    /// Wildcard origins, methods, headers, or exposed headers cannot be combined with credentials.
    #[error("CORS wildcard cannot be combined with allow_credentials(true): {0}")]
    WildcardWithCredentials(&'static str),
    /// Literal `"*"` is disallowed in explicit lists; use the explicit wildcard opt-in instead.
    #[error("literal '*' is not allowed in CORS {0}; use the explicit wildcard builder")]
    LiteralWildcardNotAllowed(&'static str),
    /// A configured origin, header name/value, or HTTP method failed validation.
    #[error("invalid CORS {field}: {reason}")]
    InvalidValue { field: &'static str, reason: String },
}

/// Typed, fail-closed CORS configuration.
///
/// By default, no cross-origin requests are permitted.
#[derive(Debug, Clone, Default)]
pub struct CorsConfig {
    /// Optional `Access-Control-Max-Age` duration for preflight responses.
    pub max_age: Option<Duration>,
    /// Whether `Access-Control-Allow-Credentials: true` is enabled (default: `false`).
    pub allow_credentials: bool,
}

impl CorsConfig {
    /// Creates a default fail-closed CORS configuration denying all cross-origin requests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates this configuration and builds a [`CorsLayer`].
    #[allow(unused_variables, dead_code)] // Removed in Task 02.
    pub fn build_layer(&self) -> Result<CorsLayer, CorsConfigError> {
        todo!("implemented in Task 02")
    }
}
