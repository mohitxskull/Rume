//! CSRF protection middleware using double-submit cookies, HMAC token signing, and constant-time token comparison.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use thiserror::Error;
use tower::{Layer, Service};
use zeroize::Zeroizing;

use crate::auth::identity::AuthUser;
use rume_foundation::error::AppError;
use rume_foundation::util::safe_equal;

/// Maximum number of matching CSRF cookie candidates inspected per request.
const MAX_CSRF_COOKIE_CANDIDATES: usize = 8;
/// Minimum HMAC secret length in bytes (256 bits) required for CSRF signing (`RC-03`).
pub const MIN_CSRF_SECRET_LEN: usize = 32;
/// Default CSRF token lifetime (12 hours).
const DEFAULT_CSRF_TOKEN_TTL: Duration = Duration::from_secs(12 * 3600);
/// Default session cookie name used to derive per-session CSRF binding context.
const DEFAULT_SESSION_COOKIE_NAME: &str = "rume_session";
/// Fallback binding context for anonymous (unauthenticated, no session cookie) requests.
const ANON_BINDING_CONTEXT: &str = "anon";
/// Small clock-skew tolerance for tokens issued slightly in the future on a peer node.
const CLOCK_SKEW_TOLERANCE_SECS: u64 = 60;

/// Configuration error returned when constructing [`CsrfConfig`] or [`CsrfLayer`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CsrfConfigError {
    /// Provided HMAC secret is shorter than [`MIN_CSRF_SECRET_LEN`] bytes.
    #[error(
        "CSRF HMAC secret must be at least {min} bytes (got {actual}); supply a high-entropy shared key or use with_random_secret()"
    )]
    SecretTooShort { min: usize, actual: usize },
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

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
    ///
    /// Full subdomain cookie-tossing protection applies to session-bound requests (carrying a
    /// configured session cookie or authenticated [`AuthUser`] extension); anonymous requests
    /// bind to `"anon"`.
    pub secret: Option<Zeroizing<Vec<u8>>>,
    /// Lifetime of signed CSRF tokens before expiration (default: 12 hours).
    pub token_ttl: Duration,
    /// Optional session cookie name used to bind CSRF tokens to the caller's session
    /// (default: `Some("rume_session")`).
    pub session_cookie_name: Option<String>,
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
            .field("token_ttl", &self.token_ttl)
            .field("session_cookie_name", &self.session_cookie_name)
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
            token_ttl: DEFAULT_CSRF_TOKEN_TTL,
            session_cookie_name: Some(DEFAULT_SESSION_COOKIE_NAME.to_string()),
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
    /// Deployments must supply a high-entropy shared key of at least [`MIN_CSRF_SECRET_LEN`] (32)
    /// bytes; use [`CsrfConfig::with_random_secret`] as an explicit alternative for single-instance
    /// setups.
    pub fn with_secret(mut self, secret: impl Into<Vec<u8>>) -> Result<Self, CsrfConfigError> {
        let secret = secret.into();
        if secret.len() < MIN_CSRF_SECRET_LEN {
            return Err(CsrfConfigError::SecretTooShort {
                min: MIN_CSRF_SECRET_LEN,
                actual: secret.len(),
            });
        }
        self.secret = Some(Zeroizing::new(secret));
        Ok(self)
    }

    /// Sets the CSRF token TTL (default: 12 hours).
    pub fn with_token_ttl(mut self, ttl: Duration) -> Self {
        self.token_ttl = ttl;
        self
    }

    /// Sets the session cookie name used to bind CSRF tokens to the caller's session.
    /// Pass `None` to skip cookie-based session lookup and fall back to [`AuthUser`] or `"anon"`.
    pub fn with_session_cookie_name(mut self, name: Option<String>) -> Self {
        self.session_cookie_name = name;
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

    /// Generates a cryptographically strong CSRF token bound to the anonymous (`"anon"`)
    /// binding context.
    ///
    /// For full subdomain cookie-tossing protection on authenticated/session-bearing requests,
    /// prefer [`Self::generate_token_for_session`].
    // Why: Tokens are represented as plain `String`/`&str` values (no `CsrfToken` wrapper struct)
    // because `CsrfService` reads and compares raw header and cookie strings directly.
    pub fn generate_token(&self) -> String {
        self.generate_token_for_session(ANON_BINDING_CONTEXT)
    }

    /// Generates a cryptographically strong CSRF token bound to `session_id` and the current
    /// timestamp (`{nonce}.{issued_at_hex}.{sig_hex}` when HMAC secret is configured).
    pub fn generate_token_for_session(&self, session_id: &str) -> String {
        self.generate_token_at(session_id, now_epoch_secs())
    }

    fn generate_token_at(&self, binding_context: &str, issued_at_secs: u64) -> String {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        if let Some(secret) = &self.secret {
            let sig = compute_csrf_hmac(secret, binding_context, issued_at_secs, &nonce);
            format!("{nonce}.{issued_at_secs:x}.{sig}")
        } else {
            nonce
        }
    }

    /// Verifies the authenticity, expiry, and signature of a token against the anonymous
    /// (`"anon"`) binding context.
    ///
    /// Fails closed if no secret is configured unless `allow_unauthenticated` is explicitly enabled.
    pub fn verify_token(&self, token: &str) -> bool {
        self.verify_token_for_session(token, ANON_BINDING_CONTEXT)
    }

    /// Verifies the authenticity, session binding, expiry, and signature of a token against
    /// `session_id`.
    ///
    /// Fails closed if no secret is configured unless `allow_unauthenticated` is explicitly enabled.
    pub fn verify_token_for_session(&self, token: &str, session_id: &str) -> bool {
        self.verify_token_at(token, session_id, now_epoch_secs())
    }

    fn verify_token_at(&self, token: &str, binding_context: &str, now_secs: u64) -> bool {
        let Some(secret) = &self.secret else {
            return self.allow_unauthenticated && !token.is_empty();
        };
        if secret.len() < MIN_CSRF_SECRET_LEN {
            return false;
        }

        let mut parts = token.split('.');
        let (Some(nonce), Some(issued_at_hex), Some(sig_hex), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        if nonce.is_empty() || issued_at_hex.is_empty() || sig_hex.is_empty() {
            return false;
        }

        let Ok(issued_at_secs) = u64::from_str_radix(issued_at_hex, 16) else {
            return false;
        };

        let ttl_secs = self.token_ttl.as_secs();
        if now_secs > issued_at_secs.saturating_add(ttl_secs) {
            return false;
        }
        if issued_at_secs > now_secs.saturating_add(CLOCK_SKEW_TOLERANCE_SECS) {
            return false;
        }

        let expected = compute_csrf_hmac(secret, binding_context, issued_at_secs, nonce);
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

fn compute_csrf_hmac(
    secret: &[u8],
    binding_context: &str,
    issued_at_secs: u64,
    nonce: &str,
) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    // Why: length-prefix `binding_context` and encode `issued_at_secs` as fixed 8-byte
    // big-endian before `nonce` so no delimiter collision can forge a cross-session MAC (`RC-03`).
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC can take key of any size");
    mac.update(&(binding_context.len() as u64).to_be_bytes());
    mac.update(binding_context.as_bytes());
    mac.update(&issued_at_secs.to_be_bytes());
    mac.update(nonce.as_bytes());
    to_hex(&mac.finalize().into_bytes())
}

fn derive_session_binding(req: &Request<Body>, config: &CsrfConfig) -> String {
    if let Some(ref session_cookie_name) = config.session_cookie_name {
        for header_val in req.headers().get_all(COOKIE) {
            if let Ok(cookies) = header_val.to_str() {
                for pair in cookies.split(';') {
                    if let Some((name, value)) = pair.trim().split_once('=')
                        && name == session_cookie_name
                        && !value.is_empty()
                    {
                        return value.to_string();
                    }
                }
            }
        }
    }

    if let Some(user) = req.extensions().get::<AuthUser>()
        && !user.id.is_empty()
    {
        return user.id.clone();
    }

    ANON_BINDING_CONTEXT.to_string()
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
    /// Creates a CSRF protection layer with an explicit HMAC secret of at least
    /// [`MIN_CSRF_SECRET_LEN`] (32) bytes.
    ///
    /// Deployments must supply a high-entropy shared key across all instances
    /// in a multi-instance deployment, otherwise CSRF validation will fail ~50%
    /// of the time behind a load balancer. Use [`CsrfLayer::with_random_secret`]
    /// as the explicit single-instance alternative.
    pub fn new(secret: impl AsRef<[u8]>) -> Result<Self, CsrfConfigError> {
        let config = CsrfConfig::default().with_secret(secret.as_ref().to_vec())?;
        Ok(Self::with_config(config))
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
    /// Panics if `config.secret` is `Some` with fewer than [`MIN_CSRF_SECRET_LEN`] bytes.
    pub fn with_config(config: CsrfConfig) -> Self {
        assert!(
            config
                .secret
                .as_ref()
                .is_none_or(|s| s.len() >= MIN_CSRF_SECRET_LEN),
            "CSRF HMAC secret must be at least {MIN_CSRF_SECRET_LEN} bytes; supply a high-entropy shared key or use CsrfLayer::with_random_secret()"
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
            let session_binding = derive_session_binding(&req, &config);

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
                        .any(|value| config.verify_token_for_session(value, &session_binding));

                    if !has_valid_cookie {
                        token_to_inject = Some(config.generate_token_for_session(&session_binding));
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
            if !config.verify_token_for_session(header_token, &session_binding) {
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
    fn test_csrf_rejects_short_or_empty_secret() {
        assert_eq!(
            CsrfLayer::new(b"").unwrap_err(),
            CsrfConfigError::SecretTooShort { min: 32, actual: 0 }
        );
        assert_eq!(
            CsrfLayer::new(b"short-secret-only-25bytes").unwrap_err(),
            CsrfConfigError::SecretTooShort {
                min: 32,
                actual: 25,
            }
        );
        assert_eq!(
            CsrfConfig::default()
                .with_secret(vec![0u8; 31])
                .unwrap_err(),
            CsrfConfigError::SecretTooShort {
                min: 32,
                actual: 31,
            }
        );
    }

    #[test]
    #[should_panic(expected = "CSRF HMAC secret must be at least 32 bytes")]
    fn test_csrf_layer_with_config_rejects_short_secret() {
        let config = CsrfConfig {
            secret: Some(Zeroizing::new(vec![1u8; 16])),
            ..Default::default()
        };
        let _ = CsrfLayer::with_config(config);
    }

    #[test]
    fn test_csrf_with_random_secret_and_explicit_secret_sign_and_verify() {
        let config = CsrfConfig::default()
            .with_secret(b"high-entropy-shared-secret-key!!".to_vec())
            .unwrap();
        let token = config.generate_token();
        assert!(config.verify_token(&token));

        let dbg = format!("{config:?}");
        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("high-entropy"));

        let _layer = CsrfLayer::with_random_secret();
        let short_struct = CsrfConfig {
            secret: Some(Zeroizing::new(vec![0u8; 16])),
            ..Default::default()
        };
        assert!(!short_struct.verify_token("anything.1.deadbeef"));
    }

    #[test]
    fn test_csrf_token_is_bound_to_session_and_expires() {
        let config = CsrfConfig::default()
            .with_secret(b"0123456789abcdef0123456789abcdef".to_vec())
            .unwrap()
            .with_token_ttl(Duration::from_secs(300));

        // Token bound to session A must be accepted for session A and rejected for session B / anon.
        let tok_a = config.generate_token_for_session("session-A");
        assert!(config.verify_token_for_session(&tok_a, "session-A"));
        assert!(!config.verify_token_for_session(&tok_a, "session-B"));
        assert!(!config.verify_token(&tok_a));

        // Expired token must be rejected.
        let issued_at = 1_700_000_000u64;
        let tok_timed = config.generate_token_at("session-A", issued_at);
        assert!(config.verify_token_at(&tok_timed, "session-A", issued_at + 300));
        assert!(!config.verify_token_at(&tok_timed, "session-A", issued_at + 301));
    }

    #[tokio::test]
    async fn test_csrf_service_rejects_cross_session_token_tossing() {
        use axum::Router;
        use axum::routing::post;
        use tower::ServiceExt;

        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        let config = CsrfConfig::default().with_secret(secret.clone()).unwrap();
        let token_for_a = config.generate_token_for_session("sess-A");

        let app = Router::new()
            .route("/mutate", post(|| async { StatusCode::OK }))
            .layer(CsrfLayer::new(secret).unwrap());

        // Same session cookie -> 200 OK.
        let resp_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mutate")
                    .header("x-csrf-token", &token_for_a)
                    .header(
                        COOKIE,
                        format!("rume_session=sess-A; csrf_token={token_for_a}"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_ok.status(), StatusCode::OK);

        // Victim session B with attacker's tossed session-A token -> 403 Forbidden.
        let resp_cross_session = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mutate")
                    .header("x-csrf-token", &token_for_a)
                    .header(
                        COOKIE,
                        format!("rume_session=sess-B; csrf_token={token_for_a}"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_cross_session.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_csrf_caps_matching_cookie_candidates() {
        use axum::Router;
        use axum::routing::post;
        use tower::ServiceExt;

        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        let config = CsrfConfig::default().with_secret(secret.clone()).unwrap();
        let valid_token = config.generate_token();

        let app = Router::new()
            .route("/mutate", post(|| async { StatusCode::OK }))
            .layer(CsrfLayer::new(secret).unwrap());

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

        let mut config = CsrfConfig::default()
            .with_secret(b"0123456789abcdef0123456789abcdef".to_vec())
            .unwrap();
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
