//! CSRF protection middleware using double-submit cookies, HMAC token signing, and constant-time token comparison.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use tower::{Layer, Service};
use zeroize::Zeroizing;

use rume_foundation::error::AppError;
use rume_foundation::util::safe_equal;

/// Maximum number of matching CSRF cookie candidates inspected per request.
const MAX_CSRF_COOKIE_CANDIDATES: usize = 8;

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// CSRF protection configuration.
#[derive(Clone)]
pub struct CsrfConfig {
    /// Header name to read the client token from (default: `x-csrf-token`).
    pub header_name: String,
    /// Cookie name storing the CSRF token (default: `csrf_token`).
    pub cookie_name: String,
    /// Set of URL path prefixes exempt from CSRF validation.
    pub exempt_prefixes: BTreeSet<String>,
    /// Optional secret key for HMAC-signing tokens to defeat subdomain cookie tossing.
    pub secret: Option<Zeroizing<Vec<u8>>>,
    /// Explicit opt-in to allow unauthenticated double-submit tokens when `secret` is None.
    /// Default is `false` (fail-closed) to protect against subdomain cookie-tossing in production.
    pub allow_unauthenticated: bool,
    /// Whether to automatically generate and inject the CSRF cookie on safe requests (`GET`, `HEAD`, `OPTIONS`, `TRACE`).
    pub auto_inject_cookie: bool,
    /// Cookie path (default: `/`).
    pub cookie_path: String,
    /// Cookie SameSite policy (default: `Strict`).
    pub cookie_same_site: String,
    /// Cookie Secure flag (default: `true`; set to `false` only for local non-TLS development/testing).
    pub cookie_secure: bool,
    /// Cookie HttpOnly flag (default: `false` so client-side JavaScript can read it for double-submit headers).
    pub cookie_http_only: bool,
}

impl std::fmt::Debug for CsrfConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsrfConfig")
            .field("header_name", &self.header_name)
            .field("cookie_name", &self.cookie_name)
            .field("exempt_prefixes", &self.exempt_prefixes)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("allow_unauthenticated", &self.allow_unauthenticated)
            .field("auto_inject_cookie", &self.auto_inject_cookie)
            .field("cookie_path", &self.cookie_path)
            .field("cookie_same_site", &self.cookie_same_site)
            .field("cookie_secure", &self.cookie_secure)
            .field("cookie_http_only", &self.cookie_http_only)
            .finish()
    }
}

impl Default for CsrfConfig {
    fn default() -> Self {
        Self {
            header_name: "x-csrf-token".to_string(),
            cookie_name: "csrf_token".to_string(),
            exempt_prefixes: BTreeSet::new(),
            secret: None,
            allow_unauthenticated: false,
            auto_inject_cookie: false,
            cookie_path: "/".to_string(),
            cookie_same_site: "Strict".to_string(),
            cookie_secure: true,
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
    ///
    /// Deployments must supply a high-entropy shared key; use [`CsrfConfig::with_random_secret`]
    /// as an explicit alternative for single-instance setups.
    ///
    /// # Panics
    ///
    /// Panics if `secret` is empty.
    pub fn with_secret(mut self, secret: impl Into<Vec<u8>>) -> Self {
        let secret = secret.into();
        assert!(
            !secret.is_empty(),
            "CSRF HMAC secret must not be empty; supply a high-entropy shared key or use with_random_secret()"
        );
        self.secret = Some(Zeroizing::new(secret));
        self
    }

    /// Auto-generates a cryptographically strong 32-byte HMAC secret for token signing.
    pub fn with_random_secret(mut self) -> Self {
        let mut secret = Vec::with_capacity(32);
        secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        self.secret = Some(Zeroizing::new(secret));
        self
    }

    /// Explicitly allow unauthenticated double-submit tokens when running without an HMAC secret.
    /// In production, tokens without HMAC signing permit subdomain cookie-tossing attacks.
    pub fn allow_unauthenticated(mut self, allow: bool) -> Self {
        self.allow_unauthenticated = allow;
        self
    }

    /// Enables or disables automatic cookie injection on safe requests (`GET`, `HEAD`, `OPTIONS`, `TRACE`).
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
    // Why: Tokens are represented as plain `String`/`&str` values (no `CsrfToken` wrapper struct)
    // because `CsrfService` reads and compares raw header and cookie strings directly.
    pub fn generate_token(&self) -> String {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        if let Some(secret) = &self.secret {
            use hmac::{Hmac, KeyInit, Mac};
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
    /// Fails closed if no secret is configured unless `allow_unauthenticated` is explicitly enabled.
    pub fn verify_token(&self, token: &str) -> bool {
        let Some(secret) = &self.secret else {
            return self.allow_unauthenticated && !token.is_empty();
        };
        if secret.is_empty() {
            return false;
        }

        let Some((raw, sig_hex)) = token.split_once('.') else {
            return false;
        };

        use hmac::{Hmac, KeyInit, Mac};
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
///
/// A secret is required to construct a layer; there is no `Default` because
/// a secret-less layer would fail closed on every mutating request
/// (RC-15 root cause: `Default()` vs `new()` contradiction).
#[derive(Debug, Clone)]
pub struct CsrfLayer {
    config: Arc<CsrfConfig>,
}

impl CsrfLayer {
    /// Creates a CSRF protection layer with an explicit HMAC secret.
    ///
    /// Deployments must supply a high-entropy shared key across all instances
    /// in a multi-instance deployment, otherwise CSRF validation will fail ~50%
    /// of the time behind a load balancer. Use [`CsrfLayer::with_random_secret`]
    /// as the explicit single-instance alternative.
    ///
    /// # Panics
    ///
    /// Panics if `secret` is empty.
    pub fn new(secret: impl AsRef<[u8]>) -> Self {
        Self::with_config(CsrfConfig::default().with_secret(secret.as_ref().to_vec()))
    }

    /// Creates a CSRF protection layer with an auto-generated 32-byte HMAC secret.
    ///
    /// This is an explicit opt-in. Prefer `CsrfLayer::new(secret)` with a
    /// shared secret in multi-instance deployments.
    pub fn with_random_secret() -> Self {
        Self::with_config(CsrfConfig::default().with_random_secret())
    }

    /// Creates a CSRF protection layer with custom configuration.
    ///
    /// # Panics
    ///
    /// Panics if `config.secret` is `Some` with an empty byte slice.
    pub fn with_config(config: CsrfConfig) -> Self {
        assert!(
            !config.secret.as_ref().is_some_and(|s| s.is_empty()),
            "CSRF HMAC secret must not be empty; supply a high-entropy shared key or use CsrfLayer::with_random_secret()"
        );
        if config.secret.is_none() && !config.allow_unauthenticated {
            tracing::warn!(
                "CsrfLayer initialized without HMAC secret or allow_unauthenticated; CSRF token verification will fail closed"
            );
        } else if config.secret.is_none() {
            tracing::warn!(
                "CsrfLayer running in unauthenticated mode (allow_unauthenticated=true); tokens will not be protected against subdomain cookie-tossing"
            );
        }
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
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

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
                    // Why: cap candidate CSRF cookies before HMAC verification on safe methods
                    // too, matching the mutating-method branch so a `GET` carrying thousands of
                    // forged CSRF cookies cannot trigger unbounded HMAC work.
                    let has_valid_cookie = req
                        .headers()
                        .get_all(COOKIE)
                        .iter()
                        .filter_map(|val| val.to_str().ok())
                        .flat_map(|cookies| cookies.split(';'))
                        .filter_map(|pair| {
                            let (name, value) = pair.trim().split_once('=')?;
                            (name == config.cookie_name).then_some(value)
                        })
                        .take(MAX_CSRF_COOKIE_CANDIDATES)
                        .any(|value| config.verify_token(value));

                    if !has_valid_cookie {
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
                let is_exempt = if exempt.ends_with('/') {
                    path.starts_with(exempt)
                } else {
                    path == exempt
                        || path
                            .strip_prefix(exempt)
                            .is_some_and(|rest| rest.starts_with('/'))
                };
                if is_exempt {
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

            // Extract matching tokens from Cookie headers, capped to prevent CPU/allocation
            // amplification from cookie-flooding attacks while tolerating subdomain cookie tossing.
            let mut matching_cookies = Vec::with_capacity(MAX_CSRF_COOKIE_CANDIDATES);
            'cookie_scan: for header_val in req.headers().get_all(COOKIE) {
                if let Ok(cookies) = header_val.to_str() {
                    for pair in cookies.split(';') {
                        if let Some((name, value)) = pair.trim().split_once('=')
                            && name == config.cookie_name
                        {
                            matching_cookies.push(value);
                            if matching_cookies.len() >= MAX_CSRF_COOKIE_CANDIDATES {
                                break 'cookie_scan;
                            }
                        }
                    }
                }
            }

            if matching_cookies.is_empty() {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_MISSING",
                    message: format!("Missing required CSRF cookie '{}'", config.cookie_name),
                };
                return Ok(err.into_response());
            }

            // Why: verify `header_token` once with HMAC before comparing against cookie
            // candidates with `safe_equal`. Any cookie equal to a verified `header_token`
            // is guaranteed to carry the same valid signature, eliminating 2x per-cookie
            // HMAC recomputation (`RC-28`).
            if !config.verify_token(header_token) {
                let err = AppError::Public {
                    status: StatusCode::FORBIDDEN,
                    code: "CSRF_TOKEN_INVALID",
                    message: "CSRF token signature verification failed".into(),
                };
                return Ok(err.into_response());
            }

            let has_valid_match = matching_cookies
                .iter()
                .any(|&cookie_token| safe_equal(header_token.as_bytes(), cookie_token.as_bytes()));

            if !has_valid_match {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "CSRF HMAC secret must not be empty")]
    fn test_csrf_layer_new_rejects_empty_secret() {
        let _ = CsrfLayer::new(b"");
    }

    #[test]
    #[should_panic(expected = "CSRF HMAC secret must not be empty")]
    fn test_csrf_config_with_secret_rejects_empty_secret() {
        let _ = CsrfConfig::default().with_secret(Vec::new());
    }

    #[test]
    #[should_panic(expected = "CSRF HMAC secret must not be empty")]
    fn test_csrf_layer_with_config_rejects_empty_secret() {
        let config = CsrfConfig {
            secret: Some(Zeroizing::new(Vec::new())),
            ..Default::default()
        };
        let _ = CsrfLayer::with_config(config);
    }

    #[test]
    fn test_csrf_with_random_secret_and_explicit_secret_sign_and_verify() {
        let config =
            CsrfConfig::default().with_secret(b"high-entropy-shared-secret-key!!".to_vec());
        let token = config.generate_token();
        assert!(config.verify_token(&token));

        let dbg = format!("{config:?}");
        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("high-entropy"));

        let _layer = CsrfLayer::with_random_secret();
        let empty_struct = CsrfConfig {
            secret: Some(Zeroizing::new(Vec::new())),
            ..Default::default()
        };
        assert!(!empty_struct.verify_token("anything.deadbeef"));
    }

    #[tokio::test]
    async fn test_csrf_caps_matching_cookie_candidates() {
        use axum::Router;
        use axum::routing::post;
        use tower::ServiceExt;

        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        let config = CsrfConfig::default().with_secret(secret.clone());
        let valid_token = config.generate_token();

        let app = Router::new()
            .route("/mutate", post(|| async { StatusCode::OK }))
            .layer(CsrfLayer::new(secret));

        // 7 tossed cookies + 1 valid cookie (8th candidate, within cap) -> 200 OK
        let mut within_cap: Vec<String> = (0..7)
            .map(|i| format!("csrf_token=tossed{i}.deadbeef"))
            .collect();
        within_cap.push(format!("csrf_token={valid_token}"));
        let resp_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mutate")
                    .header("x-csrf-token", &valid_token)
                    .header(COOKIE, within_cap.join("; "))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_ok.status(), StatusCode::OK);

        // 8 tossed cookies + 1 valid cookie (9th candidate, beyond cap) -> 403 Forbidden
        let mut beyond_cap: Vec<String> = (0..8)
            .map(|i| format!("csrf_token=tossed{i}.deadbeef"))
            .collect();
        beyond_cap.push(format!("csrf_token={valid_token}"));
        let resp_denied = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mutate")
                    .header("x-csrf-token", &valid_token)
                    .header(COOKIE, beyond_cap.join("; "))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn safe_method_auto_inject_cookie_caps_csrf_cookie_candidates() {
        use tower::ServiceExt;

        let mut config =
            CsrfConfig::default().with_secret(b"0123456789abcdef0123456789abcdef".to_vec());
        config.auto_inject_cookie = true;
        let valid_token = config.generate_token();

        let app = axum::Router::new()
            .route("/read", axum::routing::get(|| async { "ok" }))
            .layer(CsrfLayer::with_config(config));

        // 7 invalid CSRF cookies + 1 valid CSRF cookie (8th candidate, within cap) -> no Set-Cookie needed.
        let mut within_cap: Vec<String> = (0..7)
            .map(|i| format!("csrf_token=tossed{i}.deadbeef"))
            .collect();
        within_cap.push(format!("csrf_token={valid_token}"));
        let resp_within = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/read")
                    .header(COOKIE, within_cap.join("; "))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_within.status(), StatusCode::OK);
        assert!(
            resp_within.headers().get(SET_COOKIE).is_none(),
            "valid CSRF cookie within candidate cap must be recognized on GET"
        );

        // 8 invalid CSRF cookies + 1 valid CSRF cookie (9th candidate, beyond cap) -> stops at cap and injects fresh cookie.
        let mut beyond_cap: Vec<String> = (0..8)
            .map(|i| format!("csrf_token=tossed{i}.deadbeef"))
            .collect();
        beyond_cap.push(format!("csrf_token={valid_token}"));
        let resp_beyond = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/read")
                    .header(COOKIE, beyond_cap.join("; "))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_beyond.status(), StatusCode::OK);
        assert!(
            resp_beyond.headers().get(SET_COOKIE).is_some(),
            "GET must cap CSRF cookie verification at MAX_CSRF_COOKIE_CANDIDATES"
        );
    }
}
