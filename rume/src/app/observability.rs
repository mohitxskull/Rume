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
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, HttpBody};
use axum::extract::FromRequestParts;
use axum::http::Request;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue};
use axum::http::request::Parts;
use axum::response::Response;
use serde::{Deserialize, Serialize};

/// Non-panicking initializer for the global tracing subscriber.
///
/// Returns `Ok(())` on successful initialization, or `Err(err)` if a global subscriber
/// has already been set.
pub fn try_init_tracing() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).try_init()
}

/// Initialize the tracing subscriber if not already initialized.
///
/// Reads `RUST_LOG` (or defaults to `info`) to set the filtering level, then
/// installs a `fmt` subscriber with that filter. If a global subscriber has already
/// been registered (e.g. during tests or multiple reboots), this is safely ignored.
pub fn init_tracing() {
    let _ = try_init_tracing();
}

/// The standard HTTP header name for request identity: `x-request-id`.
pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Standard W3C TraceContext header name: `traceparent`.
pub static TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");

/// Standard W3C TraceContext header name: `tracestate`.
pub static TRACESTATE: HeaderName = HeaderName::from_static("tracestate");

tokio::task_local! {
    /// Active task-local W3C TraceContext propagating distributed trace context across async boundaries.
    pub static CURRENT_TRACE_CONTEXT: Option<TraceContext>;
}

/// W3C TraceContext representation propagating distributed trace identity across HTTP, jobs, and outbox.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceContext {
    /// W3C `traceparent` header string (e.g. `00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`).
    pub traceparent: String,
    /// Optional W3C `tracestate` header string.
    pub tracestate: Option<String>,
}

impl TraceContext {
    /// Returns the currently active trace context from task-local storage, if any.
    pub fn current() -> Option<Self> {
        CURRENT_TRACE_CONTEXT.try_with(|ctx| ctx.clone()).ok().flatten()
    }

    /// Extracts the 32-character hex trace ID from the `traceparent`.
    pub fn trace_id(&self) -> &str {
        self.traceparent.split('-').nth(1).unwrap_or("")
    }

    /// Extracts the 16-character hex span ID from the `traceparent`.
    pub fn span_id(&self) -> &str {
        self.traceparent.split('-').nth(2).unwrap_or("")
    }

    /// Extracts the 2-character hex flags from the `traceparent`.
    pub fn flags(&self) -> &str {
        self.traceparent.split('-').nth(3).unwrap_or("01")
    }

    /// Creates a child `TraceContext` that preserves the trace ID and flags, but generates a new random span ID.
    pub fn new_child(&self) -> Self {
        let trace_id = self.trace_id();
        let flags = self.flags();
        let new_span_id = format!("{:016x}", uuid::Uuid::new_v4().as_u128() as u64);
        Self {
            traceparent: format!("00-{trace_id}-{new_span_id}-{flags}"),
            tracestate: self.tracestate.clone(),
        }
    }

    /// Generates a new valid W3C TraceContext traceparent.
    pub fn generate() -> Self {
        let trace_id = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let span_id = format!("{:016x}", uuid::Uuid::new_v4().as_u128() as u64);
        Self {
            traceparent: format!("00-{trace_id}-{span_id}-01"),
            tracestate: None,
        }
    }

    /// Creates a `TraceContext` from an explicit `traceparent` string.
    pub fn from_traceparent(traceparent: impl Into<String>) -> Self {
        Self {
            traceparent: traceparent.into(),
            tracestate: None,
        }
    }

    /// Validates whether a string matches W3C `traceparent` specification.
    pub fn is_valid_traceparent(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 4 {
            return false;
        }
        if parts[0] != "00" {
            return false;
        }
        if parts[1].len() != 32
            || !parts[1].chars().all(|c| c.is_ascii_hexdigit())
            || parts[1] == "00000000000000000000000000000000"
        {
            return false;
        }
        if parts[2].len() != 16
            || !parts[2].chars().all(|c| c.is_ascii_hexdigit())
            || parts[2] == "0000000000000000"
        {
            return false;
        }
        if parts[3].len() != 2 || !parts[3].chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        true
    }

    /// Serializes the trace context into a persistent string preserving both `traceparent` and `tracestate`.
    pub fn to_serialized(&self) -> String {
        if let Some(ref ts) = self.tracestate {
            format!("{}\n{}", self.traceparent, ts)
        } else {
            self.traceparent.clone()
        }
    }

    /// Parses a serialized trace context string, reconstructing both `traceparent` and `tracestate`.
    pub fn parse_serialized(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some((tp, ts)) = s.split_once('\n') {
            let tp = tp.trim();
            let ts = ts.trim();
            if Self::is_valid_traceparent(tp) {
                return Some(Self {
                    traceparent: tp.to_string(),
                    tracestate: if ts.is_empty() { None } else { Some(ts.to_string()) },
                });
            }
        }
        if Self::is_valid_traceparent(s) {
            Some(Self::from_traceparent(s))
        } else {
            None
        }
    }
}

impl Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_serialized())
    }
}

impl<S> FromRequestParts<S> for TraceContext
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(ctx) = parts.extensions.get::<TraceContext>() {
            Ok(ctx.clone())
        } else {
            Ok(TraceContext::generate())
        }
    }
}

/// A unique identifier for an HTTP request.
///
/// Propagated through request extensions and extractable in Axum handlers via
/// [`FromRequestParts`].
///
/// # Example
///
/// ```
/// use rume::app::RequestId;
///
/// async fn my_handler(request_id: RequestId) -> String {
///     format!("processing request {}", request_id.0)
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

impl RequestId {
    /// Generate a new, unique UUID v4 formatted request ID backed by the OS CSPRNG.
    pub fn generate() -> Self {
        RequestId(uuid::Uuid::new_v4().to_string())
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

        let trace_context = match req.headers().get(&TRACEPARENT) {
            Some(val) => match val.to_str() {
                Ok(s) if TraceContext::is_valid_traceparent(s) => {
                    let tracestate = req
                        .headers()
                        .get(&TRACESTATE)
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string);
                    TraceContext {
                        traceparent: s.to_string(),
                        tracestate,
                    }
                }
                _ => TraceContext::generate(),
            },
            None => TraceContext::generate(),
        };

        // Propagate RequestId and TraceContext through request extensions for handlers & other layers.
        req.extensions_mut().insert(request_id.clone());
        req.extensions_mut().insert(trace_context.clone());

        let method = req.method().clone();
        let matched_path = req
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map(|mp| mp.as_str().to_string());

        let tracestate_str = trace_context.tracestate.clone();
        let span = tracing::info_span!(
            "http_request",
            request_id = %request_id,
            traceparent = %trace_context.traceparent,
            tracestate = trace_context.tracestate.as_deref().unwrap_or(""),
            method = %method,
            route = tracing::field::Empty,
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        );

        if let Some(ref route) = matched_path {
            span.record("route", route.as_str());
        }

        let mut inner = self.inner.clone();
        let req_id_str = request_id.0.clone();
        let trace_parent_str = trace_context.traceparent.clone();
        let span_clone = span.clone();
        let scope_tc = Some(trace_context);

        use tracing::Instrument;
        Box::pin(
            CURRENT_TRACE_CONTEXT.scope(
                scope_tc,
                async move {
                let start = Instant::now();

                let mut response = inner.call(req).await?;
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as u64;
                let status = response.status();

                span_clone.record("status", status.as_u16());
                span_clone.record("latency_ms", latency_ms);

                if status.is_server_error() {
                    tracing::error!(status = status.as_u16(), latency_ms, "request failed");
                } else if status.is_client_error() {
                    tracing::warn!(status = status.as_u16(), latency_ms, "request error");
                } else {
                    tracing::info!(status = status.as_u16(), latency_ms, "request completed");
                }

                // Always add X-Request-ID and traceparent to response headers
                if let Ok(hdr_val) = HeaderValue::from_str(&req_id_str) {
                    response.headers_mut().insert(X_REQUEST_ID.clone(), hdr_val);
                }
                if let Ok(hdr_val) = HeaderValue::from_str(&trace_parent_str) {
                    response.headers_mut().insert(TRACEPARENT.clone(), hdr_val);
                }
                if let Some(ref ts) = tracestate_str
                    && let Ok(hdr_val) = HeaderValue::from_str(ts)
                {
                    response.headers_mut().insert(TRACESTATE.clone(), hdr_val);
                }

                // If response is RFC 9457 Problem Details, ensure request_id is populated safely.
                let is_problem_json = response
                    .headers()
                    .get(&CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|ct| ct.starts_with("application/problem+json"))
                    .unwrap_or(false);

                if is_problem_json {
                    const MAX_PROBLEM_DETAILS_INSPECT: usize = 1024 * 1024; // 1 MiB
                    let (mut parts, body) = response.into_parts();

                    let is_oversized = parts
                        .headers
                        .get(CONTENT_LENGTH)
                        .and_then(|val| val.to_str().ok())
                        .and_then(|val| val.parse::<usize>().ok())
                        .map(|len| len > MAX_PROBLEM_DETAILS_INSPECT)
                        .unwrap_or_else(|| {
                            body.size_hint().lower() > (MAX_PROBLEM_DETAILS_INSPECT as u64)
                        });

                    if is_oversized {
                        // Never destroy or truncate oversized responses! Return untouched.
                        return Ok(Response::from_parts(parts, body));
                    }

                    match axum::body::to_bytes(body, MAX_PROBLEM_DETAILS_INSPECT).await {
                        Ok(bytes) => {
                            let mut final_bytes = bytes;
                            let mut body_modified = false;

                            if let Ok(mut json) =
                                serde_json::from_slice::<serde_json::Value>(&final_bytes)
                                && let Some(obj) = json.as_object_mut()
                            {
                                let should_inject = matches!(
                                    obj.get("request_id"),
                                    None | Some(serde_json::Value::Null)
                                );
                                if should_inject {
                                    obj.insert(
                                        "request_id".to_string(),
                                        serde_json::Value::String(req_id_str),
                                    );
                                    if let Ok(new_bytes) = serde_json::to_vec(&obj) {
                                        final_bytes = axum::body::Bytes::from(new_bytes);
                                        body_modified = true;
                                    }
                                }
                            }

                            if body_modified {
                                // If Content-Length was present, update it to match new body length
                                if parts.headers.contains_key(CONTENT_LENGTH)
                                    && let Ok(len_val) =
                                        HeaderValue::from_str(&final_bytes.len().to_string())
                                {
                                    parts.headers.insert(CONTENT_LENGTH, len_val);
                                }
                            }

                            return Ok(Response::from_parts(parts, Body::from(final_bytes)));
                        }
                        Err(_) => {
                            parts.headers.remove(CONTENT_LENGTH);
                            return Ok(Response::from_parts(parts, Body::empty()));
                        }
                    }
                }

                Ok(response)
            },
        )
        .instrument(span))
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

        let parsed = uuid::Uuid::parse_str(id1.as_str()).expect("must be valid uuid");
        assert_eq!(parsed.get_version_num(), 4);
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

    #[test]
    fn tracing_initializer_idempotency_and_try_init() {
        init_tracing();
        init_tracing();
        let second_try = try_init_tracing();
        assert!(second_try.is_err());
    }

    #[test]
    fn trace_context_serialization_and_deserialization() {
        let parent = TraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: Some("rojo=1,congo=2".to_string()),
        };

        let serialized = parent.to_serialized();
        assert_eq!(
            serialized,
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\nrojo=1,congo=2"
        );

        let parsed = TraceContext::parse_serialized(&serialized).expect("must parse successfully");
        assert_eq!(parsed.traceparent, parent.traceparent);
        assert_eq!(parsed.tracestate.as_deref(), Some("rojo=1,congo=2"));

        let child = parsed.new_child();
        assert_eq!(child.trace_id(), parent.trace_id());
        assert_ne!(child.span_id(), parent.span_id());
        assert_eq!(child.tracestate.as_deref(), Some("rojo=1,congo=2"));

        // Single-line backwards compatibility
        let single_line = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let parsed_single =
            TraceContext::parse_serialized(single_line).expect("must parse single line");
        assert_eq!(parsed_single.traceparent, single_line);
        assert_eq!(parsed_single.tracestate, None);
    }

    #[tokio::test]
    async fn observability_service_injects_and_propagates_tracestate() {
        use tower::{Layer, Service};
        let mut svc = ObservabilityLayer.layer(tower::service_fn(
            |_req: Request<Body>| async {
                Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
            },
        ));

        let req = Request::builder()
            .uri("/test")
            .header(
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            )
            .header("tracestate", "vendor=val1,other=val2")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.expect("service call succeeds");
        assert_eq!(
            resp.headers()
                .get("traceparent")
                .and_then(|h| h.to_str().ok()),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        assert_eq!(
            resp.headers()
                .get("tracestate")
                .and_then(|h| h.to_str().ok()),
            Some("vendor=val1,other=val2")
        );
    }
}
