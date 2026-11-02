//! CSRF protection middleware using double-submit cookies and constant-time token comparison.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::header::COOKIE;
use axum::http::{Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::helpers::safe_equal;

/// CSRF protection configuration.
#[derive(Debug, Clone)]
pub struct CsrfConfig {
    /// Header name to read the client token from (default: `x-csrf-token`).
    pub header_name: String,
    /// Cookie name storing the CSRF token (default: `csrf_token`).
    pub cookie_name: String,
    /// Set of URL path prefixes exempt from CSRF validation.
    pub exempt_prefixes: BTreeSet<String>,
}

impl Default for CsrfConfig {
    fn default() -> Self {
        Self {
            header_name: "x-csrf-token".to_string(),
            cookie_name: "csrf_token".to_string(),
            exempt_prefixes: BTreeSet::new(),
        }
    }
}

impl CsrfConfig {
    /// Creates a default CSRF configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets custom header name.
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header_name = header.into().to_lowercase();
        self
    }

    /// Sets custom cookie name.
    pub fn with_cookie(mut self, cookie: impl Into<String>) -> Self {
        self.cookie_name = cookie.into();
        self
    }

    /// Adds an exempt path prefix (e.g. `/api/webhooks`).
    pub fn exempt(mut self, prefix: impl Into<String>) -> Self {
        self.exempt_prefixes.insert(prefix.into());
        self
    }
}

/// Tower Layer enforcing CSRF token validation on mutating requests.
#[derive(Debug, Clone, Default)]
pub struct CsrfLayer {
    config: Arc<CsrfConfig>,
}

impl CsrfLayer {
    /// Creates a CSRF protection layer with default configuration.
    pub fn new() -> Self {
        Self {
            config: Arc::new(CsrfConfig::default()),
        }
    }

    /// Creates a CSRF protection layer with custom configuration.
    pub fn with_config(config: CsrfConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S> Layer<S> for CsrfLayer {
    type Service = CsrfService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CsrfService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// Tower service enforcing CSRF verification.
#[derive(Debug, Clone)]
pub struct CsrfService<S> {
    inner: S,
    config: Arc<CsrfConfig>,
}

impl<S> Service<Request<Body>> for CsrfService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let config = Arc::clone(&self.config);
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let method = req.method();

            // Safe HTTP methods do not mutate state and are exempt from CSRF
            if matches!(
                *method,
                Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
            ) {
                return inner.call(req).await;
            }

            let path = req.uri().path();
            for exempt in &config.exempt_prefixes {
                if path.starts_with(exempt) {
                    return inner.call(req).await;
                }
            }

            // Extract token from request header
            let Some(header_val) = req.headers().get(&config.header_name) else {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_MISSING",
                    message: format!("Missing required CSRF header '{}'", config.header_name),
                };
                return Ok(err.into_response());
            };

            let Ok(header_token) = header_val.to_str() else {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_INVALID",
                    message: "Malformed CSRF token header".into(),
                };
                return Ok(err.into_response());
            };

            // Extract token from Cookie header
            let cookie_token = req
                .headers()
                .get(COOKIE)
                .and_then(|val| val.to_str().ok())
                .and_then(|cookies| {
                    cookies.split(';').find_map(|pair| {
                        let (name, value) = pair.trim().split_once('=')?;
                        if name == config.cookie_name {
                            Some(value)
                        } else {
                            None
                        }
                    })
                });

            let Some(cookie_token) = cookie_token else {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_MISSING",
                    message: format!("Missing required CSRF cookie '{}'", config.cookie_name),
                };
                return Ok(err.into_response());
            };

            // Constant-time token verification to prevent timing side-channels
            if !safe_equal(header_token.as_bytes(), cookie_token.as_bytes()) {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_MISMATCH",
                    message: "CSRF token verification failed".into(),
                };
                return Ok(err.into_response());
            }

            inner.call(req).await
        })
    }
}
