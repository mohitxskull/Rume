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
        // Why: preserve `TraceContext` and `RequestId` already inserted by an outer
        // `ObservabilityLayer` before falling back to raw HTTP headers or generating fresh IDs
        // so HTTP-layer logs/spans and gRPC handler logs/spans correlate under one ID (`RC-07`).
        let tc = req
            .extensions()
            .get::<TraceContext>()
            .cloned()
            .or_else(TraceContext::current)
            .or_else(|| extract_trace_context_from_headers(req.headers()))
            .unwrap_or_else(TraceContext::generate);
        let request_id = req
            .extensions()
            .get::<RequestId>()
            .cloned()
            .or_else(|| extract_request_id_from_headers(req.headers()))
            .unwrap_or_else(RequestId::generate);

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Response as HttpResponse;
    use rume_observability::ObservabilityLayer;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn grpc_trace_layer_preserves_outer_observability_layer_request_id_and_trace_context() {
        type CapturedContext = (RequestId, TraceContext, Option<TraceContext>);
        let captured: Arc<Mutex<Option<CapturedContext>>> = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);

        let inner = tower::service_fn(move |req: HttpRequest<Body>| {
            let captured = Arc::clone(&captured_clone);
            async move {
                let req_id = req
                    .extensions()
                    .get::<RequestId>()
                    .cloned()
                    .expect("RequestId extension");
                let tc = req
                    .extensions()
                    .get::<TraceContext>()
                    .cloned()
                    .expect("TraceContext extension");
                let current_tc = TraceContext::current();
                *captured.lock().unwrap() = Some((req_id, tc, current_tc));
                Ok::<_, std::convert::Infallible>(HttpResponse::new(Body::empty()))
            }
        });

        let svc = ObservabilityLayer::new().layer(RumeGrpcTraceLayer.layer(inner));

        // Send a request with NO x-request-id and an incoming traceparent so ObservabilityLayer
        // generates a RequestId and derives a child TraceContext.
        let parent_tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let req = HttpRequest::builder()
            .uri("/rume.v1.Greeter/SayHello")
            .header("traceparent", parent_tp)
            .body(Body::empty())
            .unwrap();

        let resp = svc.oneshot(req).await.unwrap();
        let outer_req_id = resp
            .headers()
            .get("x-request-id")
            .expect("ObservabilityLayer sets x-request-id response header")
            .to_str()
            .unwrap()
            .to_string();
        let outer_tp = resp
            .headers()
            .get("traceparent")
            .expect("ObservabilityLayer sets traceparent response header")
            .to_str()
            .unwrap()
            .to_string();

        let (handler_req_id, handler_tc, handler_current_tc) =
            captured.lock().unwrap().clone().expect("handler invoked");
        assert_eq!(
            handler_req_id.as_str(),
            outer_req_id,
            "RumeGrpcTraceLayer must preserve ObservabilityLayer's RequestId"
        );
        assert_eq!(
            handler_tc.traceparent, outer_tp,
            "RumeGrpcTraceLayer must preserve ObservabilityLayer's child TraceContext"
        );
        assert_ne!(
            handler_tc.traceparent, parent_tp,
            "child TraceContext must not be overwritten back to raw header parent"
        );
        assert_eq!(
            handler_current_tc.map(|t| t.traceparent),
            Some(outer_tp),
            "TraceContext::current() inside gRPC handler must match ObservabilityLayer"
        );
    }
}
