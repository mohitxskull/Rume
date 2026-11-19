//! Security headers middleware injecting standard HTTP defenses.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, HeaderName, HeaderValue, STRICT_TRANSPORT_SECURITY,
};
use axum::http::{Request, Response};
use tower::{Layer, Service};

const X_FRAME_OPTIONS: &str = "x-frame-options";
const X_CONTENT_TYPE_OPTIONS: &str = "x-content-type-options";
const REFERRER_POLICY: &str = "referrer-policy";
const PERMISSIONS_POLICY: &str = "permissions-policy";
const X_XSS_PROTECTION: &str = "x-xss-protection";

/// Configuration for HTTP security response headers.
#[derive(Debug, Clone)]
pub struct SecurityHeadersConfig {
    /// Content-Security-Policy header.
    pub content_security_policy: Option<String>,
    /// X-Frame-Options header (e.g. DENY, SAMEORIGIN).
    pub x_frame_options: Option<String>,
    /// X-Content-Type-Options header (typically nosniff).
    pub x_content_type_options: Option<String>,
    /// Strict-Transport-Security header (HSTS).
    pub strict_transport_security: Option<String>,
    /// Referrer-Policy header.
    pub referrer_policy: Option<String>,
    /// Permissions-Policy header.
    pub permissions_policy: Option<String>,
    /// X-XSS-Protection header (0 is standard modern recommendation).
    pub x_xss_protection: Option<String>,
    /// Arbitrary additional security headers.
    pub custom_headers: BTreeMap<String, String>,
    /// Whether HSTS injection strictly requires HTTPS or `x-forwarded-proto: https` (default: `true`).
    pub hsts_require_tls: bool,
    /// Whether native TLS is terminated in-process (default: `false`).
    pub native_tls: bool,
    /// Whether to trust `x-forwarded-proto` and `Forwarded` headers from reverse proxies (default: `false`).
    pub trust_proxy_headers: bool,
    /// Explicit opt-in to trust proxy headers from any client IP address (default: `false`).
    pub trust_all_proxies: bool,
    /// Optional list of trusted proxy IP addresses. When non-empty, proxy headers are only honored if the client IP matches.
    pub trusted_proxies: Vec<std::net::IpAddr>,
}

impl Default for SecurityHeadersConfig {
    fn default() -> Self {
        Self {
            content_security_policy: Some("default-src 'self'".to_string()),
            x_frame_options: Some("DENY".to_string()),
            x_content_type_options: Some("nosniff".to_string()),
            strict_transport_security: Some("max-age=31536000; includeSubDomains".to_string()),
            referrer_policy: Some("strict-origin-when-cross-origin".to_string()),
            permissions_policy: Some("geolocation=(), camera=(), microphone=()".to_string()),
            x_xss_protection: Some("0".to_string()),
            custom_headers: BTreeMap::new(),
            hsts_require_tls: true,
            native_tls: false,
            trust_proxy_headers: false,
            trust_all_proxies: false,
            trusted_proxies: Vec::new(),
        }
    }
}

impl SecurityHeadersConfig {
    /// Creates a default secure configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets Content-Security-Policy.
    pub fn with_csp(mut self, csp: impl Into<String>) -> Self {
        self.content_security_policy = Some(csp.into());
        self
    }

    /// Sets X-Frame-Options.
    pub fn with_frame_options(mut self, opt: impl Into<String>) -> Self {
        self.x_frame_options = Some(opt.into());
        self
    }

    /// Sets Strict-Transport-Security.
    pub fn with_hsts(mut self, hsts: impl Into<String>) -> Self {
        self.strict_transport_security = Some(hsts.into());
        self
    }

    /// Configures whether HSTS injection requires HTTPS or `x-forwarded-proto: https` (default: `true`).
    pub fn with_hsts_require_tls(mut self, require: bool) -> Self {
        self.hsts_require_tls = require;
        self
    }

    /// Flags that native TLS is terminated in-process.
    pub fn with_native_tls(mut self, native: bool) -> Self {
        self.native_tls = native;
        self
    }

    /// Configures whether reverse proxy headers (`x-forwarded-proto`, `Forwarded`) are trusted.
    pub fn with_trust_proxy_headers(mut self, trust: bool) -> Self {
        self.trust_proxy_headers = trust;
        self
    }

    /// Explicitly opts in to trusting reverse proxy headers from any client IP address.
    pub fn with_trust_all_proxies(mut self, trust: bool) -> Self {
        self.trust_all_proxies = trust;
        if trust {
            self.trust_proxy_headers = true;
        }
        self
    }

    /// Adds a trusted reverse proxy IP address and enables proxy header evaluation.
    pub fn with_trusted_proxy(mut self, ip: std::net::IpAddr) -> Self {
        self.trust_proxy_headers = true;
        self.trusted_proxies.push(ip);
        self
    }

    /// Sets the full list of trusted reverse proxy IP addresses and enables proxy header evaluation.
    pub fn with_trusted_proxies(mut self, ips: Vec<std::net::IpAddr>) -> Self {
        self.trust_proxy_headers = true;
        self.trusted_proxies = ips;
        self
    }

    /// Adds a custom header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_headers
            .insert(name.into().to_lowercase(), value.into());
        self
    }
}

/// Tower Layer injecting security headers onto all outgoing responses.
#[derive(Debug, Clone, Default)]
pub struct SecurityHeadersLayer {
    config: Arc<SecurityHeadersConfig>,
}

impl SecurityHeadersLayer {
    /// Creates a security headers layer with default hardened settings.
    pub fn new() -> Self {
        Self {
            config: Arc::new(SecurityHeadersConfig::default()),
        }
    }

    /// Creates a security headers layer with custom configuration.
    pub fn with_config(config: SecurityHeadersConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S> Layer<S> for SecurityHeadersLayer {
    type Service = SecurityHeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityHeadersService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// Tower service that applies security headers to responses.
#[derive(Debug, Clone)]
pub struct SecurityHeadersService<S> {
    inner: S,
    config: Arc<SecurityHeadersConfig>,
}

impl<S> Service<Request<Body>> for SecurityHeadersService<S>
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

        let client_ip = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip());
        let proxy_trusted = config.trust_proxy_headers
            && (config.trust_all_proxies
                || (!config.trusted_proxies.is_empty()
                    && client_ip
                        .map(|ip| config.trusted_proxies.contains(&ip))
                        .unwrap_or(false)));

        let is_https = config.native_tls
            || req.uri().scheme() == Some(&axum::http::uri::Scheme::HTTPS)
            || (proxy_trusted && forwarded_headers_indicate_https(req.headers()));

        Box::pin(async move {
            let mut response = inner.call(req).await?;
            let headers = response.headers_mut();

            if let Some(csp) = &config.content_security_policy
                && !headers.contains_key(CONTENT_SECURITY_POLICY)
                && let Ok(val) = HeaderValue::from_str(csp)
            {
                headers.insert(CONTENT_SECURITY_POLICY, val);
            }

            if let Some(hsts) = &config.strict_transport_security
                && (!config.hsts_require_tls || is_https)
                && !headers.contains_key(STRICT_TRANSPORT_SECURITY)
                && let Ok(val) = HeaderValue::from_str(hsts)
            {
                headers.insert(STRICT_TRANSPORT_SECURITY, val);
            }

            let apply_named =
                |headers: &mut axum::http::HeaderMap, name: &'static str, opt: &Option<String>| {
                    if let Some(val_str) = opt
                        && let Ok(header_name) = HeaderName::from_bytes(name.as_bytes())
                        && !headers.contains_key(&header_name)
                        && let Ok(val) = HeaderValue::from_str(val_str)
                    {
                        headers.insert(header_name, val);
                    }
                };

            apply_named(headers, X_FRAME_OPTIONS, &config.x_frame_options);
            apply_named(
                headers,
                X_CONTENT_TYPE_OPTIONS,
                &config.x_content_type_options,
            );
            apply_named(headers, REFERRER_POLICY, &config.referrer_policy);
            apply_named(headers, PERMISSIONS_POLICY, &config.permissions_policy);
            apply_named(headers, X_XSS_PROTECTION, &config.x_xss_protection);

            for (name, val_str) in &config.custom_headers {
                if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes())
                    && !headers.contains_key(&header_name)
                    && let Ok(val) = HeaderValue::from_str(val_str)
                {
                    headers.insert(header_name, val);
                }
            }

            Ok(response)
        })
    }
}

/// Checks whether reverse-proxy `X-Forwarded-Proto` or RFC 7239 `Forwarded`
/// headers indicate HTTPS on the rightmost (closest trusted proxy) hop.
///
/// Why: per RFC 9110 §5.3, multiple header lines are folded in order so the
/// closest trusted proxy appends at the end; evaluating only the first header
/// or any hop (`.any(...)`) lets a client spoof `proto=https` in an earlier hop
/// or inside a quoted `Forwarded` parameter value (e.g. `host="a;proto=https"`).
fn forwarded_headers_indicate_https(headers: &axum::http::HeaderMap) -> bool {
    // 1. Check rightmost non-empty value across all `X-Forwarded-Proto` headers.
    let mut xfp_rightmost: Option<&str> = None;
    for val in headers.get_all("x-forwarded-proto") {
        if let Ok(s) = val.to_str() {
            for part in s.split(',') {
                let trimmed = part.trim();
                if !trimmed.is_empty() {
                    xfp_rightmost = Some(trimmed);
                }
            }
        }
    }
    if xfp_rightmost.is_some_and(|p| p.eq_ignore_ascii_case("https")) {
        return true;
    }

    // 2. Check rightmost RFC 7239 `Forwarded` element across all `Forwarded` headers.
    let mut rightmost_element: Option<&str> = None;
    for val in headers.get_all("forwarded") {
        if let Ok(s) = val.to_str() {
            for elem in split_unquoted(s, ',') {
                let trimmed = elem.trim();
                if !trimmed.is_empty() {
                    rightmost_element = Some(trimmed);
                }
            }
        }
    }
    if let Some(elem) = rightmost_element {
        for param in split_unquoted(elem, ';') {
            if let Some((k, raw_v)) = param.trim().split_once('=')
                && k.trim().eq_ignore_ascii_case("proto")
            {
                let v = unquote_rfc7239(raw_v.trim());
                return v.eq_ignore_ascii_case("https");
            }
        }
    }

    false
}

/// Splits `input` by `delim` while ignoring delimiters inside double-quoted strings
/// (respecting `\"` and `\\` escapes per RFC 7239 / RFC 9110).
fn split_unquoted(input: &str, delim: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut start = 0;

    for (idx, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delim {
            parts.push(&input[start..idx]);
            start = idx + ch.len_utf8();
        }
    }
    parts.push(&input[start..]);
    parts
}

fn unquote_rfc7239(raw: &str) -> String {
    if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                out.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::Service;

    #[tokio::test]
    async fn test_forwarded_parameter_parsing_and_proxy_trust() {
        use std::net::SocketAddr;
        use tower::Layer;

        let config = SecurityHeadersConfig::new().with_trusted_proxy("10.0.0.1".parse().unwrap());
        let mut service = SecurityHeadersLayer::with_config(config).layer(tower::service_fn(
            |_req: Request<Body>| async {
                Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
            },
        ));

        // 1. Host with proto=https in host parameter must NOT trigger HSTS
        let mut req1 = Request::builder()
            .header("forwarded", "for=192.0.2.60;host=\"proto=https.evil.com\"")
            .body(Body::empty())
            .unwrap();
        req1.extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::from((
                [10, 0, 0, 1],
                1234,
            ))));

        let res1 = service.call(req1).await.unwrap();
        assert!(!res1.headers().contains_key("strict-transport-security"));

        // 2. Untrusted proxy socket IP must not trigger HSTS
        let mut req2 = Request::builder()
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();
        req2.extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::from((
                [192, 168, 1, 1],
                1234,
            ))));

        let res2 = service.call(req2).await.unwrap();
        assert!(!res2.headers().contains_key("strict-transport-security"));

        // 3. Trusted proxy socket IP with valid proto=https triggers HSTS
        let mut req3 = Request::builder()
            .header("forwarded", "for=192.0.2.60;proto=https")
            .body(Body::empty())
            .unwrap();
        req3.extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::from((
                [10, 0, 0, 1],
                1234,
            ))));

        let res3 = service.call(req3).await.unwrap();
        assert!(res3.headers().contains_key("strict-transport-security"));
    }

    #[test]
    fn forwarded_header_parsing_respects_quotes_and_rightmost_hop() {
        let mut h1 = axum::http::HeaderMap::new();
        h1.append(
            "forwarded",
            HeaderValue::from_static(r#"for=192.0.2.60;host="a;proto=https""#),
        );
        assert!(
            !forwarded_headers_indicate_https(&h1),
            "proto=https inside quoted host parameter must not be treated as HTTPS"
        );

        let mut h2 = axum::http::HeaderMap::new();
        h2.append(
            "forwarded",
            HeaderValue::from_static("proto=https, for=10.0.0.1;proto=http"),
        );
        assert!(
            !forwarded_headers_indicate_https(&h2),
            "earlier hop proto=https must not override rightmost hop proto=http"
        );

        let mut h3 = axum::http::HeaderMap::new();
        h3.append("x-forwarded-proto", HeaderValue::from_static("https"));
        h3.append("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(
            !forwarded_headers_indicate_https(&h3),
            "rightmost X-Forwarded-Proto header wins"
        );

        let mut h4 = axum::http::HeaderMap::new();
        h4.append(
            "forwarded",
            HeaderValue::from_static("for=10.0.0.1;proto=http"),
        );
        h4.append(
            "forwarded",
            HeaderValue::from_static(r#"for=10.0.0.2;host="x,y";proto="https""#),
        );
        assert!(forwarded_headers_indicate_https(&h4));
    }
}
