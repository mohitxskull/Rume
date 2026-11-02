//! Request identity and observability middleware.
//!
//! Provides:
//! - Safe `X-Request-ID` extraction or automatic UUID v4 generation.
//! - Request extension propagation and [`RequestId`] extractor.
//! - Automatic inclusion of `request_id` in RFC 9457 Problem Details responses.
//! - Structured tracing spans with request ID, method, path, status, and duration.
//! - Sanitized logging: no request bodies, credentials, or sensitive headers are logged.

use std::fmt::{self, Display};
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::http::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use axum::http::request::Parts;
use axum::http::Request;
use axum::response::Response;
use serde::{Deserialize, Serialize};

/// The standard HTTP header name for request identity: `x-request-id`.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// A unique identifier for an HTTP request.
///
/// Propagated through request extensions and extractable in Axum handlers via
/// [`FromRequestParts`].
///
/// # Example
///
/// ```ignore
/// use rume::app::RequestId;
///
/// async fn my_handler(request_id: RequestId) -> String {
///     format!("processing request {request_id}")
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

impl RequestId {
    /// Generate a new, unique UUID v4 formatted request ID.
    pub fn generate() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos();
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);

        // Mix timestamp nanos, counter, and constant seed with splitmix64
        let mut r1 = splitmix64((nanos as u64) ^ count);
        let mut r2 = splitmix64((nanos >> 64) as u64 ^ count.rotate_left(32) ^ 0x9e3779b97f4a7c15);

        // UUID v4 format: set version (4) and variant (RFC 4122)
        r1 = (r1 & 0xffffffffffff0fff) | 0x0000000000004000;
        r2 = (r2 & 0x3fffffffffffffff) | 0x8000000000000000;

        let b1 = r1.to_be_bytes();
        let b2 = r2.to_be_bytes();

        let id = format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b1[0], b1[1], b1[2], b1[3],
            b1[4], b1[5],
            b1[6], b1[7],
            b2[0], b2[1],
            b2[2], b2[3], b2[4], b2[5], b2[6], b2[7],
        );
        RequestId(id)
    }

    /// Access the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for RequestId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for RequestId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl<S> FromRequestParts<S> for RequestId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(id) = parts.extensions.get::<RequestId>() {
            Ok(id.clone())
        } else {
            Ok(RequestId::generate())
        }
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Validate an incoming `X-Request-ID` value.
///
/// Ensures the value is non-empty, at most 128 characters, and contains only
/// safe ASCII characters (alphanumeric, `-`, `_`, `.`, `/`).
fn is_valid_request_id(value: &str) -> bool {
    let len = value.len();
    if !(1..=128).contains(&len) {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/')
}

/// A Tower layer that attaches request IDs, records tracing spans, and includes
/// request IDs in responses.
#[derive(Debug, Clone, Default)]
pub struct ObservabilityLayer;

impl ObservabilityLayer {
    /// Create a new observability layer.
    pub fn new() -> Self {
        Self
    }
}

impl<S> tower::Layer<S> for ObservabilityLayer {
    type Service = ObservabilityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ObservabilityService { inner }
    }
}

/// Tower service that manages request identity, observability spans, and response headers.
#[derive(Debug, Clone)]
pub struct ObservabilityService<S> {
    inner: S,
}

impl<S> tower::Service<Request<Body>> for ObservabilityService<S>
where
    S: tower::Service<Request<Body>, Response = Response> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let request_id = match req.headers().get(&X_REQUEST_ID) {
            Some(val) => match val.to_str() {
                Ok(s) if is_valid_request_id(s) => RequestId(s.to_string()),
                _ => RequestId::generate(),
            },
            None => RequestId::generate(),
        };

        // Propagate RequestId through request extensions for handlers & other layers.
        req.extensions_mut().insert(request_id.clone());

        let method = req.method().clone();
        let path = req.uri().path().to_string();

        let span = tracing::info_span!(
            "http_request",
            request_id = %request_id,
            method = %method,
            path = %path,
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        );

        let mut inner = self.inner.clone();
        let req_id_str = request_id.0.clone();

        Box::pin(async move {
            let start = Instant::now();
            let _enter = span.enter();

            let mut response = inner.call(req).await?;
            let elapsed = start.elapsed();
            let latency_ms = elapsed.as_millis() as u64;
            let status = response.status();

            span.record("status", status.as_u16());
            span.record("latency_ms", latency_ms);

            if status.is_server_error() {
                tracing::error!(status = status.as_u16(), latency_ms, "request failed");
            } else if status.is_client_error() {
                tracing::warn!(status = status.as_u16(), latency_ms, "request error");
            } else {
                tracing::info!(status = status.as_u16(), latency_ms, "request completed");
            }

            // Always add X-Request-ID to response headers
            if let Ok(hdr_val) = HeaderValue::from_str(&req_id_str) {
                response.headers_mut().insert(X_REQUEST_ID.clone(), hdr_val);
            }

            // If response is RFC 9457 Problem Details, ensure request_id is populated
            let is_problem_json = response
                .headers()
                .get(&CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.starts_with("application/problem+json"))
                .unwrap_or(false);

            if is_problem_json {
                let (parts, body) = response.into_parts();
                if let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await {
                    if let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && let Some(obj) = json.as_object_mut()
                    {
                        let should_inject = obj
                            .get("request_id")
                            .map(|v| v.is_null())
                            .unwrap_or(true);
                        if should_inject {
                            obj.insert(
                                "request_id".to_string(),
                                serde_json::Value::String(req_id_str),
                            );
                            if let Ok(new_bytes) = serde_json::to_vec(&obj) {
                                return Ok(Response::from_parts(parts, Body::from(new_bytes)));
                            }
                        }
                    }
                    return Ok(Response::from_parts(parts, Body::from(bytes)));
                }
                return Ok(Response::from_parts(parts, Body::empty()));
            }

            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_generates_valid_uuid_format() {
        let id1 = RequestId::generate();
        let id2 = RequestId::generate();

        assert_ne!(id1, id2);
        assert_eq!(id1.len(), 36);
        assert_eq!(id1.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn validates_incoming_request_ids() {
        assert!(is_valid_request_id("valid-request-id-123"));
        assert!(is_valid_request_id("c1f7b0a8-0000-4000-8000-000000000000"));
        assert!(!is_valid_request_id("")); // empty
        assert!(!is_valid_request_id("bad id with spaces"));
        assert!(!is_valid_request_id("bad\nid"));
        assert!(!is_valid_request_id(&"a".repeat(129))); // too long
    }
}
