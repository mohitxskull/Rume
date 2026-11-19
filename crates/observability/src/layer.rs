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

        let incoming_tc = req
            .headers()
            .get(&TRACEPARENT)
            .and_then(|v| v.to_str().ok())
            .filter(|s| TraceContext::is_valid_traceparent(s))
            .map(|tp| {
                let tracestate = req
                    .headers()
                    .get(&TRACESTATE)
                    .and_then(|v| v.to_str().ok())
                    .map(ToString::to_string);
                TraceContext {
                    traceparent: tp.to_string(),
                    tracestate,
                }
            });

        // Why: extract into an explicit empty `opentelemetry::Context::new()` while temporarily
        // attaching `Context::new()` as current. In `opentelemetry 0.33`,
        // `NoopTextMapPropagator::extract_with_context` ignores its `cx` argument and returns
        // `Context::current()`, so without detaching the ambient thread-local context, any request
        // arriving while a global propagator has not yet been registered would inherit ambient traces.
        let propagated_cx = {
            let _isolated = opentelemetry::Context::new().attach();
            opentelemetry::global::get_text_map_propagator(|prop| {
                prop.extract_with_context(
                    &opentelemetry::Context::new(),
                    &HeaderMapExtractor(req.headers()),
                )
            })
        };
        let parent_cx = if propagated_cx.span().span_context().is_valid() {
            propagated_cx
        } else if let Some(ref tc) = incoming_tc {
            tc.apply_to_otel_context(&propagated_cx)
        } else {
            propagated_cx
        };

        let method = req.method().clone();
        let matched_path = req
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map(|mp| mp.as_str().to_string());

        let span = tracing::info_span!(
            "http_request",
            request_id = %request_id,
            traceparent = tracing::field::Empty,
            tracestate = tracing::field::Empty,
            method = %method,
            route = tracing::field::Empty,
            status = tracing::field::Empty,
            latency_ms = tracing::field::Empty,
        );

        // Why: unconditionally call `set_parent(parent_cx)` BEFORE `span.context()`. When an
        // `OpenTelemetryLayer` is installed, `on_new_span` captures ambient `OtelContext::current()`
        // on the span builder, so passing `Context::new()` for unheadered requests clears any
        // ambient parent while headered requests bind the extracted parent; calling `span.context()`
        // first would start the span (`AlreadyStarted`) and silently reject `set_parent`.
        let _ = span.set_parent(parent_cx.clone());
        let span_cx = span.context();

        let (server_tc, exec_cx) = if let Some(tc) = TraceContext::from_otel_context(&span_cx) {
            (tc, span_cx)
        } else {
            // Why: when no `OpenTelemetryLayer` is active, `span.context()` is invalid; derive a
            // fresh child from `parent_cx` (preserving traces accepted by the global propagator
            // such as future W3C versions) or a new root when `parent_cx` has no valid span, and
            // layer the child span context onto `parent_cx` so propagated baggage is retained.
            let tc = TraceContext::from_otel_context(&parent_cx)
                .map_or_else(TraceContext::generate, |parent| parent.new_child());
            let cx = tc.apply_to_otel_context(&parent_cx);
            (tc, cx)
        };

        span.record("traceparent", server_tc.traceparent.as_str());
        span.record("tracestate", server_tc.tracestate.as_deref().unwrap_or(""));

        // Propagate RequestId and server span TraceContext through request extensions.
        req.extensions_mut().insert(request_id.clone());
        req.extensions_mut().insert(server_tc.clone());

        if let Some(ref route) = matched_path {
            span.record("route", route.as_str());
        }

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let req_id_str = request_id.0.clone();
        let trace_parent_str = server_tc.traceparent.clone();
        let tracestate_str = server_tc.tracestate.clone();
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
            .with_context(exec_cx)
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
        let incoming_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let incoming_span_id = "00f067aa0ba902b7";
        let captured_handler_tp = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let captured_clone = std::sync::Arc::clone(&captured_handler_tp);

        let mut svc = ObservabilityLayer.layer(tower::service_fn(move |req: Request<Body>| {
            let captured = std::sync::Arc::clone(&captured_clone);
            async move {
                let current =
                    TraceContext::current().expect("TraceContext::current() must be active");
                let ext = req
                    .extensions()
                    .get::<TraceContext>()
                    .expect("TraceContext extension must be inserted");
                assert_eq!(&current, ext);
                assert_eq!(current.trace_id(), incoming_trace_id);
                assert_ne!(current.span_id(), incoming_span_id);
                assert_eq!(
                    current.tracestate.as_deref(),
                    Some("vendor=val1,other=val2")
                );
                *captured.lock().unwrap() = Some(current.traceparent);
                Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
            }
        }));

        let req = Request::builder()
            .uri("/test")
            .header(
                "traceparent",
                format!("00-{incoming_trace_id}-{incoming_span_id}-01"),
            )
            .header("tracestate", "vendor=val1,other=val2")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.expect("service call succeeds");
        let handler_tp = captured_handler_tp.lock().unwrap().clone().unwrap();
        assert_eq!(
            resp.headers()
                .get("traceparent")
                .and_then(|h| h.to_str().ok()),
            Some(handler_tp.as_str())
        );
        assert_eq!(
            resp.headers()
                .get("tracestate")
                .and_then(|h| h.to_str().ok()),
            Some("vendor=val1,other=val2")
        );
    }

    #[tokio::test]
    async fn observability_layer_emits_child_span_id_with_and_without_otel_subscriber() {
        use opentelemetry::propagation::TextMapPropagator;
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_sdk::propagation::TraceContextPropagator;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use tracing::Instrument as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        let incoming_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let incoming_span_id = "00f067aa0ba902b7";
        let incoming_tp = format!("00-{incoming_trace_id}-{incoming_span_id}-01");

        // (a) Incoming traceparent without OpenTelemetryLayer
        let no_otel_captured = std::sync::Arc::new(std::sync::Mutex::new(None::<TraceContext>));
        let no_otel_captured_clone = std::sync::Arc::clone(&no_otel_captured);
        let mut no_otel_svc =
            ObservabilityLayer.layer(tower::service_fn(move |req: Request<Body>| {
                let cap = std::sync::Arc::clone(&no_otel_captured_clone);
                async move {
                    let current = TraceContext::current().expect("TraceContext::current() active");
                    let ext = req
                        .extensions()
                        .get::<TraceContext>()
                        .expect("TraceContext extension active")
                        .clone();
                    assert_eq!(current, ext);
                    *cap.lock().unwrap() = Some(current);
                    Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
                }
            }));
        let req = Request::builder()
            .uri("/no-otel")
            .header("traceparent", &incoming_tp)
            .header("tracestate", "k=v")
            .body(Body::empty())
            .unwrap();
        let resp = no_otel_svc.call(req).await.unwrap();
        let no_otel_tc = no_otel_captured.lock().unwrap().clone().unwrap();
        assert_eq!(no_otel_tc.trace_id(), incoming_trace_id);
        assert_ne!(no_otel_tc.span_id(), incoming_span_id);
        assert_eq!(no_otel_tc.tracestate.as_deref(), Some("k=v"));
        assert_eq!(
            resp.headers()
                .get("traceparent")
                .and_then(|v| v.to_str().ok()),
            Some(no_otel_tc.traceparent.as_str())
        );

        // (b) Incoming traceparent + unheadered isolation WITH explicit SdkTracerProvider + OpenTelemetryLayer
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("layer-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        let _sub_guard = tracing::subscriber::set_default(subscriber);

        let otel_captured = std::sync::Arc::new(std::sync::Mutex::new(None::<TraceContext>));
        let otel_captured_clone = std::sync::Arc::clone(&otel_captured);
        let mut otel_svc =
            ObservabilityLayer.layer(tower::service_fn(move |req: Request<Body>| {
                let cap = std::sync::Arc::clone(&otel_captured_clone);
                async move {
                    let current = TraceContext::current().expect("TraceContext::current() active");
                    let ext = req
                        .extensions()
                        .get::<TraceContext>()
                        .expect("TraceContext extension active")
                        .clone();
                    assert_eq!(current, ext);
                    *cap.lock().unwrap() = Some(current);
                    Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
                }
            }));

        // Note: simulate incoming request with traceparent while an ambient OTel context is active
        let ambient_trace_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let ambient_tc =
            TraceContext::from_traceparent(format!("00-{ambient_trace_id}-1111111111111111-01"));
        let ambient_cx = TraceContextPropagator::new().extract(&ambient_tc);

        let headered_req = Request::builder()
            .uri("/with-otel")
            .header("traceparent", &incoming_tp)
            .header("tracestate", "vendor=val1")
            .body(Body::empty())
            .unwrap();
        let headered_resp = async { otel_svc.call(headered_req).await }
            .with_context(ambient_cx.clone())
            .await
            .unwrap();
        let headered_tc = otel_captured.lock().unwrap().clone().unwrap();
        assert_eq!(headered_tc.trace_id(), incoming_trace_id);
        assert_ne!(headered_tc.span_id(), incoming_span_id);
        assert_eq!(headered_tc.tracestate.as_deref(), Some("vendor=val1"));
        assert_eq!(
            headered_resp
                .headers()
                .get("traceparent")
                .and_then(|v| v.to_str().ok()),
            Some(headered_tc.traceparent.as_str())
        );

        // Unheadered request executed under an ambient OTel context must start an isolated root trace
        let ambient_span = tracing::info_span!("ambient_caller");
        let _ = ambient_span.set_parent(ambient_cx.clone());
        let unheadered_req = Request::builder()
            .uri("/unheadered")
            .body(Body::empty())
            .unwrap();
        let unheadered_resp = async { otel_svc.call(unheadered_req).await }
            .with_context(ambient_cx)
            .instrument(ambient_span)
            .await
            .unwrap();
        let unheadered_tc = otel_captured.lock().unwrap().clone().unwrap();
        assert_ne!(
            unheadered_tc.trace_id(),
            ambient_trace_id,
            "unheadered request must not inherit ambient OtelContext::current()"
        );
        assert_eq!(
            unheadered_resp
                .headers()
                .get("traceparent")
                .and_then(|v| v.to_str().ok()),
            Some(unheadered_tc.traceparent.as_str())
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

    #[tokio::test]
    async fn observability_fallback_preserves_future_trace_version_and_baggage() {
        use opentelemetry::baggage::BaggageExt as _;
        use opentelemetry::propagation::TextMapCompositePropagator;
        use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};

        // Install a composite propagator (trace + baggage) without any OpenTelemetryLayer.
        let composite = TextMapCompositePropagator::new(vec![
            Box::new(TraceContextPropagator::new()),
            Box::new(BaggagePropagator::new()),
        ]);
        opentelemetry::global::set_text_map_propagator(composite);

        let observed_baggage = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let observed_trace = std::sync::Arc::new(std::sync::Mutex::new(None::<TraceContext>));
        let obs_b = std::sync::Arc::clone(&observed_baggage);
        let obs_t = std::sync::Arc::clone(&observed_trace);

        let mut svc = ObservabilityLayer.layer(tower::service_fn(move |_req: Request<Body>| {
            let obs_b = std::sync::Arc::clone(&obs_b);
            let obs_t = std::sync::Arc::clone(&obs_t);
            async move {
                let cx = opentelemetry::Context::current();
                let region = cx.baggage().get("region").map(|v| v.to_string());
                *obs_b.lock().unwrap() = region;
                *obs_t.lock().unwrap() = TraceContext::current();
                Ok::<Response<Body>, std::convert::Infallible>(Response::new(Body::empty()))
            }
        }));

        // Version-02 traceparent is accepted by SDK 0.33.1's TraceContextPropagator even though
        // `TraceContext::is_valid_traceparent` only validates version `00`.
        let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let parent_span_id = "00f067aa0ba902b7";
        let req = Request::builder()
            .uri("/baggage-and-v02")
            .header("traceparent", format!("02-{trace_id}-{parent_span_id}-01"))
            .header("baggage", "region=west")
            .body(Body::empty())
            .unwrap();

        let resp = svc.call(req).await.unwrap();
        let resp_tp = resp
            .headers()
            .get(&TRACEPARENT)
            .and_then(|v| v.to_str().ok())
            .expect("response must carry traceparent");
        let resp_tc = TraceContext::from_traceparent(resp_tp);
        assert_eq!(
            resp_tc.trace_id(),
            trace_id,
            "fallback child must inherit trace ID from parent_cx"
        );
        assert_ne!(
            resp_tc.span_id(),
            parent_span_id,
            "fallback child must mint a fresh server span ID"
        );

        assert_eq!(
            observed_baggage.lock().unwrap().as_deref(),
            Some("west"),
            "fallback execution context must retain propagated baggage"
        );
        let handler_tc = observed_trace
            .lock()
            .unwrap()
            .clone()
            .expect("handler must see active TraceContext");
        assert_eq!(handler_tc.trace_id(), trace_id);
        assert_eq!(handler_tc.span_id(), resp_tc.span_id());
    }
}
