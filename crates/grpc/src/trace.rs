//! W3C TraceContext and RequestId propagation for tonic gRPC.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::http::{HeaderMap, Request as HttpRequest};
use rume_foundation::RequestId;
use rume_observability::TraceContext;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status};
use tower::{Layer, Service};

/// Tonic [`Interceptor`] that extracts/injects W3C `traceparent`/`tracestate` and `x-request-id`
/// for synchronous metadata/extension propagation.
///
/// For async task-local [`TraceContext::current()`] scoping inside gRPC handlers, wrap your
/// gRPC server/router with [`RumeGrpcTraceLayer`].
#[derive(Debug, Clone, Default)]
pub struct RumeTraceInterceptor;

impl RumeTraceInterceptor {
    /// Extracts a [`TraceContext`] from gRPC [`MetadataMap`] if a valid `traceparent` is present.
    pub fn extract_from_metadata(metadata: &MetadataMap) -> Option<TraceContext> {
        let traceparent = metadata.get("traceparent")?.to_str().ok()?.trim();
        if !TraceContext::is_valid_traceparent(traceparent) {
            return None;
        }
        let tracestate = metadata
            .get("tracestate")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        Some(TraceContext {
            traceparent: traceparent.to_string(),
            tracestate,
        })
    }

    /// Injects a [`TraceContext`] into gRPC [`MetadataMap`].
    pub fn inject_into_metadata(tc: &TraceContext, metadata: &mut MetadataMap) {
        if let Ok(val) = MetadataValue::try_from(tc.traceparent.as_str()) {
            metadata.insert(MetadataKey::from_static("traceparent"), val);
        }
        if let Some(ref ts) = tc.tracestate
            && let Ok(val) = MetadataValue::try_from(ts.as_str())
        {
            metadata.insert(MetadataKey::from_static("tracestate"), val);
        }
    }

    /// Extracts a validated [`RequestId`] from gRPC [`MetadataMap`] (`x-request-id`).
    pub fn extract_request_id(metadata: &MetadataMap) -> Option<RequestId> {
        metadata
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| rume_observability::is_valid_request_id(s))
            .map(RequestId::from)
    }

    /// Injects a [`RequestId`] into gRPC [`MetadataMap`] (`x-request-id`).
    pub fn inject_request_id(request_id: &RequestId, metadata: &mut MetadataMap) {
        if let Ok(val) = MetadataValue::try_from(request_id.as_str()) {
            metadata.insert(MetadataKey::from_static("x-request-id"), val);
        }
    }
}

impl Interceptor for RumeTraceInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        let tc = Self::extract_from_metadata(req.metadata())
            .or_else(TraceContext::current)
            .unwrap_or_else(TraceContext::generate);
        let request_id =
            Self::extract_request_id(req.metadata()).unwrap_or_else(RequestId::generate);

        Self::inject_into_metadata(&tc, req.metadata_mut());
        Self::inject_request_id(&request_id, req.metadata_mut());

        req.extensions_mut().insert(tc);
        req.extensions_mut().insert(request_id);
        Ok(req)
    }
}

fn extract_trace_context_from_headers(headers: &HeaderMap) -> Option<TraceContext> {
    let traceparent = headers.get("traceparent")?.to_str().ok()?.trim();
    if !TraceContext::is_valid_traceparent(traceparent) {
        return None;
    }
    let tracestate = headers
        .get("tracestate")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(TraceContext {
        traceparent: traceparent.to_string(),
        tracestate,
    })
}

fn extract_request_id_from_headers(headers: &HeaderMap) -> Option<RequestId> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| rume_observability::is_valid_request_id(s))
        .map(RequestId::from)
}

/// Tower [`Layer`] that extracts W3C `traceparent`/`tracestate` and `x-request-id` from
/// incoming gRPC HTTP headers, inserts [`TraceContext`] and [`RequestId`] into request
/// extensions, and scopes the downstream async service future with [`TraceContext::scope_future`]
/// so [`TraceContext::current()`] returns `Some(...)` inside async gRPC handlers (`RC-24`).
#[derive(Debug, Clone, Default)]
pub struct RumeGrpcTraceLayer;

impl<S> Layer<S> for RumeGrpcTraceLayer {
    type Service = RumeGrpcTraceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RumeGrpcTraceService { inner }
    }
}

/// Tower [`Service`] produced by [`RumeGrpcTraceLayer`].
#[derive(Debug, Clone)]
pub struct RumeGrpcTraceService<S> {
    inner: S,
}

impl<S, ReqBody> Service<HttpRequest<ReqBody>> for RumeGrpcTraceService<S>
where
    S: Service<HttpRequest<ReqBody>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: HttpRequest<ReqBody>) -> Self::Future {
        let tc = extract_trace_context_from_headers(req.headers())
            .or_else(TraceContext::current)
            .unwrap_or_else(TraceContext::generate);
        let request_id =
            extract_request_id_from_headers(req.headers()).unwrap_or_else(RequestId::generate);

        let span = tracing::info_span!(
            "grpc.request",
            grpc.path = %req.uri().path(),
            request_id = %request_id.as_str(),
            traceparent = %tc.traceparent,
        );

        req.extensions_mut().insert(tc.clone());
        req.extensions_mut().insert(request_id);

        let fut = self.inner.call(req);
        Box::pin(tc.scope_future(span, fut))
    }
}
