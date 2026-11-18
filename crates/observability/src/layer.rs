//! Tower middleware for request identity, W3C TraceContext propagation, and sanitized spans.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, HttpBody};
use axum::http::Request;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue};
use axum::response::Response;
use futures_util::StreamExt;
use opentelemetry::context::FutureExt as OtelFutureExt;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt;
use rume_foundation::telemetry::{RequestId, X_REQUEST_ID, is_valid_request_id};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::trace_context::{TRACEPARENT, TRACESTATE, TraceContext};

struct HeaderMapExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// A Tower layer that attaches request IDs, propagates W3C TraceContext via OpenTelemetry,
/// records tracing spans, and includes request/trace IDs in responses.
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

        let propagated_cx = opentelemetry::global::get_text_map_propagator(|prop| {
            prop.extract(&HeaderMapExtractor(req.headers()))
        });
        let otel_cx = if propagated_cx.span().span_context().is_valid() {
            propagated_cx
        } else {
            trace_context.to_otel_context()
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

        let _ = span.set_parent(otel_cx.clone());

        if let Some(ref route) = matched_path {
            span.record("route", route.as_str());
        }

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let req_id_str = request_id.0.clone();
        let trace_parent_str = trace_context.traceparent.clone();
        let span_clone = span.clone();

        Box::pin(
            async move {
                let start = Instant::now();

                let mut response = inner.call(req).await?;
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as u64;
                let status = response.status();

                span_clone.record("status", status.as_u16());
                span_clone.record("latency_ms", latency_ms);
                if let Some(mp) = response.extensions().get::<axum::extract::MatchedPath>() {
                    span_clone.record("route", mp.as_str());
                }

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

                    let should_inspect = match parts
                        .headers
                        .get(CONTENT_LENGTH)
                        .and_then(|val| val.to_str().ok())
                        .and_then(|val| val.parse::<usize>().ok())
                    {
                        Some(len) => len <= MAX_PROBLEM_DETAILS_INSPECT,
                        None => body
                            .size_hint()
                            .upper()
                            .map(|u| u <= MAX_PROBLEM_DETAILS_INSPECT as u64)
                            .unwrap_or(false),
                    };

                    if !should_inspect {
                        // Never destroy or truncate oversized or streaming responses! Return untouched.
                        return Ok(Response::from_parts(parts, body));
                    }

                    // BODY INTEGRITY & OBSERVABILITY INVARIANT (CRITICAL — NEVER RETURN `Body::empty()`):
                    // When inspecting `application/problem+json` to inject `request_id`, we must NEVER destroy
                    // or truncate the response body if buffering fails or if the stream exceeds limits.
                    let mut stream = body.into_data_stream();
                    let mut collected: Vec<axum::body::Bytes> = Vec::new();
                    let mut total_len = 0;
                    let mut stream_err: Option<axum::Error> = None;
                    let mut limit_exceeded = false;

                    while let Some(chunk_res) = stream.next().await {
                        match chunk_res {
                            Ok(chunk) => {
                                total_len += chunk.len();
                                collected.push(chunk);
                                if total_len > MAX_PROBLEM_DETAILS_INSPECT {
                                    limit_exceeded = true;
                                    break;
                                }
                            }
                            Err(err) => {
                                stream_err = Some(err);
                                break;
                            }
                        }
                    }

                    if limit_exceeded || stream_err.is_some() {
                        let collected_stream =
                            futures_util::stream::iter(collected.into_iter().map(Ok));
                        let err_stream =
                            futures_util::stream::iter(stream_err.into_iter().map(Err));
                        let chained = collected_stream.chain(err_stream).chain(stream);
                        return Ok(Response::from_parts(parts, Body::from_stream(chained)));
                    }

                    let mut final_bytes = {
                        let mut buf = Vec::with_capacity(total_len);
                        for c in collected {
                            buf.extend_from_slice(&c);
                        }
                        axum::body::Bytes::from(buf)
                    };
                    let mut body_modified = false;

                    if let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&final_bytes)
                        && let Some(obj) = json.as_object_mut()
                    {
                        let should_inject =
                            matches!(obj.get("request_id"), None | Some(serde_json::Value::Null));
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

                    if body_modified
                        && parts.headers.contains_key(CONTENT_LENGTH)
                        && let Ok(len_val) = HeaderValue::from_str(&final_bytes.len().to_string())
                    {
                        parts.headers.insert(CONTENT_LENGTH, len_val);
                    }

                    return Ok(Response::from_parts(parts, Body::from(final_bytes)));
                }

                Ok(response)
            }
            .with_context(otel_cx)
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::{Layer, Service};

    #[tokio::test]
    async fn observability_layer_extracts_w3c_traceparent_into_otel_span_and_response() {
        let mut svc = ObservabilityLayer.layer(tower::service_fn(|_req: Request<Body>| async {
            let current = TraceContext::current().expect("TraceContext::current() must be active");
            assert_eq!(current.trace_id(), "4bf92f3577b34da6a3ce929d0e0e4736");
            assert_eq!(
                current.tracestate.as_deref(),
                Some("vendor=val1,other=val2")
            );
            Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
        }));

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

    #[tokio::test]
    async fn observability_service_records_matched_path_from_response_extension() {
        use axum::routing::get;
        use tower::ServiceExt;

        let router = axum::Router::new()
            .route("/users/{id}", get(|| async { "user" }))
            .route_layer(axum::middleware::from_fn(
                |req: axum::extract::Request, next: axum::middleware::Next| async move {
                    if let Some(mp) = req
                        .extensions()
                        .get::<axum::extract::MatchedPath>()
                        .cloned()
                    {
                        tracing::Span::current().record("route", mp.as_str());
                        let mut resp = next.run(req).await;
                        resp.extensions_mut().insert(mp);
                        resp
                    } else {
                        next.run(req).await
                    }
                },
            ))
            .layer(ObservabilityLayer::new());

        let req = Request::builder()
            .uri("/users/42")
            .body(Body::empty())
            .unwrap();

        let resp = router.oneshot(req).await.expect("service call succeeds");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.extensions()
                .get::<axum::extract::MatchedPath>()
                .map(|m| m.as_str()),
            Some("/users/{id}")
        );
    }

    #[tokio::test]
    async fn observability_service_preserves_body_on_invalid_problem_details_json() {
        let raw_payload = b"this is not valid json problem details";
        let mut svc =
            ObservabilityLayer.layer(tower::service_fn(move |_req: Request<Body>| async move {
                let resp = Response::builder()
                    .header(CONTENT_TYPE, "application/problem+json")
                    .body(Body::from(raw_payload.as_slice()))
                    .unwrap();
                Ok::<Response<Body>, std::convert::Infallible>(resp)
            }));

        let req = Request::builder()
            .uri("/error")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            bytes.as_ref(),
            raw_payload,
            "original body must be preserved when JSON inspection cannot parse it"
        );
    }

    #[tokio::test]
    async fn observability_service_preserves_body_on_oversized_stream() {
        let chunk_size = 64 * 1024;
        let num_chunks = 24;
        let chunks: Vec<axum::body::Bytes> = (0..num_chunks)
            .map(|i| axum::body::Bytes::from(vec![(i % 256) as u8; chunk_size]))
            .collect();
        let expected_total = num_chunks * chunk_size;

        let chunks_clone = chunks.clone();
        let mut svc = ObservabilityLayer.layer(tower::service_fn(move |_req: Request<Body>| {
            let stream = futures_util::stream::iter(
                chunks_clone
                    .clone()
                    .into_iter()
                    .map(Ok::<_, std::io::Error>),
            );
            async move {
                let resp = Response::builder()
                    .header(CONTENT_TYPE, "application/problem+json")
                    .body(Body::from_stream(stream))
                    .unwrap();
                Ok::<Response<Body>, std::convert::Infallible>(resp)
            }
        }));

        let req = Request::builder()
            .uri("/oversized")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            bytes.len(),
            expected_total,
            "oversized body must never be truncated or replaced with empty body"
        );
    }

    #[tokio::test]
    async fn observability_service_injects_request_id_into_valid_problem_details() {
        let raw_problem = serde_json::json!({
            "type": "https://example.com/probs/out-of-credit",
            "title": "You do not have enough credit.",
            "status": 403
        });
        let raw_bytes = serde_json::to_vec(&raw_problem).unwrap();

        let mut svc = ObservabilityLayer.layer(tower::service_fn(move |_req: Request<Body>| {
            let bytes = raw_bytes.clone();
            async move {
                let resp = Response::builder()
                    .header(CONTENT_TYPE, "application/problem+json")
                    .body(Body::from(bytes))
                    .unwrap();
                Ok::<Response<Body>, std::convert::Infallible>(resp)
            }
        }));

        let req = Request::builder()
            .uri("/credit-check")
            .header("x-request-id", "test-req-id-777")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["request_id"], "test-req-id-777");
        assert_eq!(parsed["title"], "You do not have enough credit.");
    }

    #[tokio::test]
    async fn observability_service_preserves_stream_error_on_inspected_problem_details() {
        let mut svc = ObservabilityLayer.layer(tower::service_fn(|_req: Request<Body>| {
            let stream = futures_util::stream::iter(vec![
                Ok(axum::body::Bytes::from_static(b"{\"title\":\"partial\"")),
                Err(std::io::Error::other("simulated mid-stream failure")),
            ]);
            async move {
                let resp = Response::builder()
                    .header(CONTENT_TYPE, "application/problem+json")
                    .header(CONTENT_LENGTH, "100")
                    .body(Body::from_stream(stream))
                    .unwrap();
                Ok::<Response<Body>, std::convert::Infallible>(resp)
            }
        }));

        let req = Request::builder()
            .uri("/stream-error")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.unwrap();
        let res = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await;
        assert!(
            res.is_err(),
            "mid-stream error must be preserved and propagated to body consumer"
        );
    }
}
