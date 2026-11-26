//! W3C TraceContext and RequestId propagation for tonic gRPC.

use rume_foundation::RequestId;
use rume_observability::TraceContext;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tonic::service::Interceptor;
use tonic::{Request, Status};

/// Tonic [`Interceptor`] that extracts/injects W3C `traceparent`/`tracestate` and `x-request-id`.
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

    /// Extracts a [`RequestId`] from gRPC [`MetadataMap`] (`x-request-id`).
    pub fn extract_request_id(metadata: &MetadataMap) -> Option<RequestId> {
        metadata
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
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
