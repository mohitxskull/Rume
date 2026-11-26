//! Fail-closed CORS configuration and layer builder backed by `tower-http`.

use std::str::FromStr;
use std::time::Duration;

use axum::http::Method;
use axum::http::header::{HeaderName, HeaderValue};
use tower_http::cors::Any;

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

/// Allowed values for a CORS header dimension (origins, methods, headers, or exposed headers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CorsAllowList {
    /// Permit no values (fail-closed default).
    #[default]
    None,
    /// Permit any value (`*`). Cannot be combined with `allow_credentials = true`.
    Any,
    /// Permit only the explicitly configured values. Literal `"*"` entries are rejected.
    List(Vec<String>),
}

/// Typed, fail-closed CORS configuration.
///
/// By default, no cross-origin requests are permitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorsConfig {
    /// Allowed origins (default: [`CorsAllowList::None`]).
    pub allow_origins: CorsAllowList,
    /// Allowed HTTP methods (default: [`CorsAllowList::None`]).
    pub allow_methods: CorsAllowList,
    /// Allowed request headers (default: [`CorsAllowList::None`]).
    pub allow_headers: CorsAllowList,
    /// Exposed response headers (default: [`CorsAllowList::None`]).
    pub expose_headers: CorsAllowList,
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

    /// Sets the explicit list of allowed origins.
    pub fn allow_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_origins = CorsAllowList::List(origins.into_iter().map(Into::into).collect());
        self
    }

    /// Adds a single allowed origin.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        match &mut self.allow_origins {
            CorsAllowList::List(list) => list.push(origin.into()),
            slot => *slot = CorsAllowList::List(vec![origin.into()]),
        }
        self
    }

    /// Explicitly opts in to allowing any origin (`Access-Control-Allow-Origin: *`).
    ///
    /// Fails closed in [`Self::build_layer`] if `allow_credentials` is also enabled.
    pub fn allow_any_origin(mut self) -> Self {
        self.allow_origins = CorsAllowList::Any;
        self
    }

    /// Sets the explicit list of allowed HTTP methods.
    pub fn allow_methods<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_methods = CorsAllowList::List(methods.into_iter().map(Into::into).collect());
        self
    }

    /// Explicitly opts in to allowing any HTTP method (`*`).
    ///
    /// Fails closed in [`Self::build_layer`] if `allow_credentials` is also enabled.
    pub fn allow_any_method(mut self) -> Self {
        self.allow_methods = CorsAllowList::Any;
        self
    }

    /// Sets the explicit list of allowed request headers.
    pub fn allow_headers<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_headers = CorsAllowList::List(headers.into_iter().map(Into::into).collect());
        self
    }

    /// Explicitly opts in to allowing any request header (`*`).
    ///
    /// Fails closed in [`Self::build_layer`] if `allow_credentials` is also enabled.
    pub fn allow_any_header(mut self) -> Self {
        self.allow_headers = CorsAllowList::Any;
        self
    }

    /// Sets the explicit list of exposed response headers.
    pub fn expose_headers<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.expose_headers = CorsAllowList::List(headers.into_iter().map(Into::into).collect());
        self
    }

    /// Explicitly opts in to exposing any response header (`*`).
    ///
    /// Fails closed in [`Self::build_layer`] if `allow_credentials` is also enabled.
    pub fn expose_any_header(mut self) -> Self {
        self.expose_headers = CorsAllowList::Any;
        self
    }

    /// Enables or disables `Access-Control-Allow-Credentials: true`.
    pub fn allow_credentials(mut self, allow: bool) -> Self {
        self.allow_credentials = allow;
        self
    }

    /// Sets the `Access-Control-Max-Age` duration for preflight responses.
    pub fn max_age(mut self, duration: Duration) -> Self {
        self.max_age = Some(duration);
        self
    }

    /// Validates this configuration and builds a [`CorsLayer`].
    pub fn build_layer(&self) -> Result<CorsLayer, CorsConfigError> {
        // Why: `tower-http` panics at runtime when `allow_credentials(true)` is
        // combined with any wildcard (`*`) rule; reject invalid combinations
        // fail-closed here so startup/config validation returns a typed error.
        if self.allow_credentials {
            if matches!(self.allow_origins, CorsAllowList::Any) {
                return Err(CorsConfigError::WildcardWithCredentials("allow_origins"));
            }
            if matches!(self.allow_methods, CorsAllowList::Any) {
                return Err(CorsConfigError::WildcardWithCredentials("allow_methods"));
            }
            if matches!(self.allow_headers, CorsAllowList::Any) {
                return Err(CorsConfigError::WildcardWithCredentials("allow_headers"));
            }
            if matches!(self.expose_headers, CorsAllowList::Any) {
                return Err(CorsConfigError::WildcardWithCredentials("expose_headers"));
            }
        }

        let mut layer = tower_http::cors::CorsLayer::new();

        match &self.allow_origins {
            CorsAllowList::None => {}
            CorsAllowList::Any => {
                layer = layer.allow_origin(Any);
            }
            CorsAllowList::List(origins) => {
                let parsed = parse_origins(origins)?;
                layer = layer.allow_origin(parsed);
            }
        }

        match &self.allow_methods {
            CorsAllowList::None => {}
            CorsAllowList::Any => {
                layer = layer.allow_methods(Any);
            }
            CorsAllowList::List(methods) => {
                let parsed = parse_methods(methods)?;
                layer = layer.allow_methods(parsed);
            }
        }

        match &self.allow_headers {
            CorsAllowList::None => {}
            CorsAllowList::Any => {
                layer = layer.allow_headers(Any);
            }
            CorsAllowList::List(headers) => {
                let parsed = parse_header_names("allow_headers", headers)?;
                layer = layer.allow_headers(parsed);
            }
        }

        match &self.expose_headers {
            CorsAllowList::None => {}
            CorsAllowList::Any => {
                layer = layer.expose_headers(Any);
            }
            CorsAllowList::List(headers) => {
                let parsed = parse_header_names("expose_headers", headers)?;
                layer = layer.expose_headers(parsed);
            }
        }

        if self.allow_credentials {
            layer = layer.allow_credentials(true);
        }

        if let Some(max_age) = self.max_age {
            layer = layer.max_age(max_age);
        }

        Ok(layer)
    }
}

fn reject_literal_wildcard(field: &'static str, value: &str) -> Result<(), CorsConfigError> {
    if value.trim() == "*" {
        return Err(CorsConfigError::LiteralWildcardNotAllowed(field));
    }
    if value.trim().is_empty() {
        return Err(CorsConfigError::InvalidValue {
            field,
            reason: "value must not be empty".to_string(),
        });
    }
    Ok(())
}

fn parse_origins(origins: &[String]) -> Result<Vec<HeaderValue>, CorsConfigError> {
    origins
        .iter()
        .map(|origin| {
            reject_literal_wildcard("allow_origins", origin)?;
            // Why: `http::HeaderValue::from_str` permits obs-text bytes (0x80..=0xFF),
            // but RFC 6454 / Fetch `Origin` values must be ASCII (IDNs use punycode).
            if !origin.is_ascii() {
                return Err(CorsConfigError::InvalidValue {
                    field: "allow_origins",
                    reason: "origin must contain only ASCII characters".to_string(),
                });
            }
            HeaderValue::from_str(origin).map_err(|err| CorsConfigError::InvalidValue {
                field: "allow_origins",
                reason: err.to_string(),
            })
        })
        .collect()
}

fn parse_methods(methods: &[String]) -> Result<Vec<Method>, CorsConfigError> {
    methods
        .iter()
        .map(|method| {
            reject_literal_wildcard("allow_methods", method)?;
            Method::from_bytes(method.as_bytes()).map_err(|err| CorsConfigError::InvalidValue {
                field: "allow_methods",
                reason: err.to_string(),
            })
        })
        .collect()
}

fn parse_header_names(
    field: &'static str,
    headers: &[String],
) -> Result<Vec<HeaderName>, CorsConfigError> {
    headers
        .iter()
        .map(|header_name| {
            reject_literal_wildcard(field, header_name)?;
            HeaderName::from_str(header_name).map_err(|err| CorsConfigError::InvalidValue {
                field,
                reason: err.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use axum::routing::get;
    use tower::ServiceExt;

    use crate::transport::cors::{CorsConfig, CorsConfigError};

    fn test_router(layer: crate::transport::cors::CorsLayer) -> Router {
        Router::new()
            .route("/ping", get(|| async { "pong" }))
            .layer(layer)
    }

    #[tokio::test]
    async fn cors_default_denies_cross_origin_requests() {
        let layer = CorsConfig::new()
            .build_layer()
            .expect("default fail-closed CORS config must build");
        let app = test_router(layer);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/ping")
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "default CORS config must not emit Access-Control-Allow-Origin"
        );
    }

    #[tokio::test]
    async fn cors_allows_configured_origin_and_preflight_headers() {
        let layer = CorsConfig::new()
            .allow_origins(["https://app.example"])
            .allow_methods(["GET", "POST"])
            .allow_headers(["content-type", "authorization"])
            .expose_headers(["x-request-id"])
            .max_age(Duration::from_secs(600))
            .build_layer()
            .expect("valid CORS config must build");
        let app = test_router(layer);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/ping")
                    .header(header::ORIGIN, "https://app.example")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            response.status() == StatusCode::OK || response.status() == StatusCode::NO_CONTENT,
            "unexpected preflight status: {}",
            response.status()
        );
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&header::HeaderValue::from_static("https://app.example"))
        );
        let allow_methods = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .expect("missing Access-Control-Allow-Methods")
            .to_str()
            .unwrap();
        assert!(allow_methods.contains("GET"));
        assert!(allow_methods.contains("POST"));
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_MAX_AGE),
            Some(&header::HeaderValue::from_static("600"))
        );
    }

    #[test]
    fn cors_rejects_wildcard_origin_with_credentials() {
        let err = CorsConfig::new()
            .allow_any_origin()
            .allow_credentials(true)
            .build_layer()
            .expect_err("wildcard origin with credentials must fail closed");
        assert_eq!(
            err,
            CorsConfigError::WildcardWithCredentials("allow_origins")
        );

        let method_err = CorsConfig::new()
            .allow_origins(["https://app.example"])
            .allow_any_method()
            .allow_credentials(true)
            .build_layer()
            .expect_err("wildcard methods with credentials must fail closed");
        assert_eq!(
            method_err,
            CorsConfigError::WildcardWithCredentials("allow_methods")
        );

        let header_err = CorsConfig::new()
            .allow_origins(["https://app.example"])
            .allow_any_header()
            .allow_credentials(true)
            .build_layer()
            .expect_err("wildcard headers with credentials must fail closed");
        assert_eq!(
            header_err,
            CorsConfigError::WildcardWithCredentials("allow_headers")
        );
    }

    #[test]
    fn cors_rejects_wildcard_expose_headers_with_credentials_and_literal_star_in_lists() {
        let expose_err = CorsConfig::new()
            .allow_origins(["https://app.example"])
            .expose_any_header()
            .allow_credentials(true)
            .build_layer()
            .expect_err("wildcard expose_headers with credentials must fail closed");
        assert_eq!(
            expose_err,
            CorsConfigError::WildcardWithCredentials("expose_headers")
        );

        assert_eq!(
            CorsConfig::new()
                .allow_origins(["*"])
                .build_layer()
                .expect_err("literal '*' in allow_origins must fail closed"),
            CorsConfigError::LiteralWildcardNotAllowed("allow_origins")
        );
        assert_eq!(
            CorsConfig::new()
                .allow_methods(["*"])
                .build_layer()
                .expect_err("literal '*' in allow_methods must fail closed"),
            CorsConfigError::LiteralWildcardNotAllowed("allow_methods")
        );
        assert_eq!(
            CorsConfig::new()
                .allow_headers(["*"])
                .build_layer()
                .expect_err("literal '*' in allow_headers must fail closed"),
            CorsConfigError::LiteralWildcardNotAllowed("allow_headers")
        );
        assert_eq!(
            CorsConfig::new()
                .expose_headers(["*"])
                .build_layer()
                .expect_err("literal '*' in expose_headers must fail closed"),
            CorsConfigError::LiteralWildcardNotAllowed("expose_headers")
        );
    }

    #[test]
    fn cors_rejects_invalid_origin_header_value() {
        let crlf_err = CorsConfig::new()
            .allow_origins(["https://good.example\r\nX-Injected: 1"])
            .build_layer()
            .expect_err("CRLF origin must fail closed");
        assert!(matches!(
            crlf_err,
            CorsConfigError::InvalidValue {
                field: "allow_origins",
                ..
            }
        ));

        let non_ascii_err = CorsConfig::new()
            .allow_origins(["https://exämple.test"])
            .build_layer()
            .expect_err("non-ASCII origin must fail closed");
        assert!(matches!(
            non_ascii_err,
            CorsConfigError::InvalidValue {
                field: "allow_origins",
                ..
            }
        ));
    }
}
