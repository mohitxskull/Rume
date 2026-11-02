//! CSRF protection middleware using double-submit cookies, HMAC token signing, and constant-time token comparison.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::helpers::safe_equal;

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Helper token representation holding an issued CSRF token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrfToken(pub String);

impl CsrfToken {
    /// Creates a new `CsrfToken`.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Access the underlying token string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// CSRF protection configuration.
#[derive(Debug, Clone)]
pub struct CsrfConfig {
    /// Header name to read the client token from (default: `x-csrf-token`).
    pub header_name: String,
    /// Cookie name storing the CSRF token (default: `csrf_token`).
    pub cookie_name: String,
    /// Set of URL path prefixes exempt from CSRF validation.
    pub exempt_prefixes: BTreeSet<String>,
    /// Optional secret key for HMAC-signing tokens to defeat subdomain cookie tossing.
    pub secret: Option<Vec<u8>>,
    /// Whether to automatically generate and inject the CSRF cookie on safe GET/HEAD requests.
    pub auto_inject_cookie: bool,
    /// Cookie path (default: `/`).
    pub cookie_path: String,
    /// Cookie SameSite policy (default: `Strict`).
    pub cookie_same_site: String,
    /// Cookie Secure flag (default: `false` for development/testing flexibility; recommended `true` in production).
    pub cookie_secure: bool,
    /// Cookie HttpOnly flag (default: `false` so client-side JavaScript can read it for double-submit headers).
    pub cookie_http_only: bool,
}

impl Default for CsrfConfig {
    fn default() -> Self {
        Self {
            header_name: "x-csrf-token".to_string(),
            cookie_name: "csrf_token".to_string(),
            exempt_prefixes: BTreeSet::new(),
            secret: None,
            auto_inject_cookie: false,
            cookie_path: "/".to_string(),
            cookie_same_site: "Strict".to_string(),
            cookie_secure: false,
            cookie_http_only: false,
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

    /// Sets an HMAC secret key for token signing and validation, preventing subdomain cookie tossing.
    pub fn with_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        self.secret = Some(secret.into());
        self
    }

    /// Enables or disables automatic cookie injection on safe GET/HEAD requests.
    pub fn with_auto_inject_cookie(mut self, auto_inject: bool) -> Self {
        self.auto_inject_cookie = auto_inject;
        self
    }

    /// Sets the cookie path attribute (default: `/`).
    pub fn with_cookie_path(mut self, path: impl Into<String>) -> Self {
        self.cookie_path = path.into();
        self
    }

    /// Sets the cookie SameSite attribute (default: `Strict`).
    pub fn with_same_site(mut self, same_site: impl Into<String>) -> Self {
        self.cookie_same_site = same_site.into();
        self
    }

    /// Sets the cookie Secure flag.
    pub fn with_secure(mut self, secure: bool) -> Self {
        self.cookie_secure = secure;
        self
    }

    /// Sets the cookie HttpOnly flag.
    pub fn with_http_only(mut self, http_only: bool) -> Self {
        self.cookie_http_only = http_only;
        self
    }

    /// Generates a cryptographically strong CSRF token, signing it with HMAC if a secret is configured.
    pub fn generate_token(&self) -> String {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        if let Some(secret) = &self.secret {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;

            let mut mac =
                HmacSha256::new_from_slice(secret).expect("HMAC can take key of any size");
            mac.update(raw.as_bytes());
            let sig = to_hex(&mac.finalize().into_bytes());
            format!("{raw}.{sig}")
        } else {
            raw
        }
    }

    /// Verifies the authenticity and signature of a token.
    pub fn verify_token(&self, token: &str) -> bool {
        let Some(secret) = &self.secret else {
            return !token.is_empty();
        };

        let Some((raw, sig_hex)) = token.split_once('.') else {
            return false;
        };

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
            return false;
        };
        mac.update(raw.as_bytes());
        let expected = to_hex(&mac.finalize().into_bytes());

        safe_equal(expected.as_bytes(), sig_hex.as_bytes())
    }

    /// Formats a `Set-Cookie` header value for the given CSRF token.
    pub fn make_cookie_header(&self, token: &str) -> String {
        let mut cookie = format!(
            "{}={}; Path={}; SameSite={}",
            self.cookie_name, token, self.cookie_path, self.cookie_same_site
        );
        if self.cookie_secure {
            cookie.push_str("; Secure");
        }
        if self.cookie_http_only {
            cookie.push_str("; HttpOnly");
        }
        cookie
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
                // If auto_inject_cookie is enabled, ensure the client has a valid cookie token
                let mut token_to_inject = None;
                if config.auto_inject_cookie {
                    let existing_cookie = req
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

                    let is_valid = match existing_cookie {
                        Some(tok) => config.verify_token(tok),
                        None => false,
                    };

                    if !is_valid {
                        token_to_inject = Some(config.generate_token());
                    }
                }

                let mut res = inner.call(req).await?;
                if let Some(tok) = token_to_inject {
                    let cookie_str = config.make_cookie_header(&tok);
                    if let Ok(val) = HeaderValue::from_str(&cookie_str) {
                        res.headers_mut().append(SET_COOKIE, val);
                    }
                }
                return Ok(res);
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

            // Verify cookie signature if secret is configured (guards against subdomain cookie tossing)
            if !config.verify_token(cookie_token) {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_INVALID",
                    message: "CSRF cookie signature verification failed".into(),
                };
                return Ok(err.into_response());
            }

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
