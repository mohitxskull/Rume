//! Optional tonic gRPC integration with W3C TraceContext propagation and
//! `AppError` mapping for Rume.

#![forbid(unsafe_code)]

pub mod error;
pub mod router;
pub mod trace;

pub use error::app_error_to_status;
pub use router::{GrpcRouterExt, mount_grpc_routes};
pub use trace::{RumeGrpcTraceLayer, RumeGrpcTraceService, RumeTraceInterceptor};

#[cfg(test)]
mod tests {
    use rume_foundation::RequestId;
    use rume_foundation::error::{AppError, InternalSource};
    use rume_observability::TraceContext;
    use tonic::metadata::MetadataMap;
    use tonic::service::Interceptor as _;
    use tonic::{Code, Request};

    use super::*;

    #[test]
    fn grpc_maps_app_error_to_status_and_propagates_w3c_trace_context() {
        assert_eq!(
            app_error_to_status(&AppError::NotFound).code(),
            Code::NotFound
        );
        assert_eq!(
            app_error_to_status(&AppError::Unauthorized).code(),
            Code::Unauthenticated
        );
        assert_eq!(
            app_error_to_status(&AppError::Forbidden).code(),
            Code::PermissionDenied
        );
        assert_eq!(
            app_error_to_status(&AppError::bad_request("bad input")).code(),
            Code::InvalidArgument
        );
        assert_eq!(
            app_error_to_status(&AppError::conflict("duplicate")).code(),
            Code::AlreadyExists
        );
        let internal_status = app_error_to_status(&AppError::Internal {
            source: InternalSource::new("secret db password"),
            request_id: None,
        });
        assert_eq!(internal_status.code(), Code::Internal);
        assert!(
            !internal_status.message().contains("secret db password"),
            "internal error details must not leak in gRPC status"
        );

        let mut metadata = MetadataMap::new();
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let ts = "rojo=1";
        metadata.insert("traceparent", tp.parse().unwrap());
        metadata.insert("tracestate", ts.parse().unwrap());
        metadata.insert("x-request-id", "req-123".parse().unwrap());

        let req = Request::from_parts(metadata, tonic::Extensions::new(), ());
        let mut interceptor = RumeTraceInterceptor;
        let intercepted = interceptor
            .call(req)
            .expect("RumeTraceInterceptor must accept valid metadata");

        let extracted_tc = intercepted
            .extensions()
            .get::<TraceContext>()
            .expect("TraceContext must be inserted into request extensions");
        assert_eq!(extracted_tc.traceparent, tp);
        assert_eq!(extracted_tc.tracestate.as_deref(), Some(ts));

        let extracted_req_id = intercepted
            .extensions()
            .get::<RequestId>()
            .expect("RequestId must be inserted into request extensions");
        assert_eq!(extracted_req_id.as_str(), "req-123");

        // Also test injection into MetadataMap.
        let mut out_meta = MetadataMap::new();
        RumeTraceInterceptor::inject_into_metadata(extracted_tc, &mut out_meta);
        assert_eq!(
            out_meta.get("traceparent").and_then(|v| v.to_str().ok()),
            Some(tp)
        );
        assert_eq!(
            out_meta.get("tracestate").and_then(|v| v.to_str().ok()),
            Some(ts)
        );
    }

    #[tokio::test]
    async fn grpc_trace_layer_scopes_task_local_trace_context_in_async_service() {
        use std::convert::Infallible;
        use tower::{Layer, ServiceExt};

        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let svc = RumeGrpcTraceLayer.layer(tower::service_fn(
            |req: axum::http::Request<()>| async move {
                let current_tc = TraceContext::current()
                    .expect("TraceContext::current() must be Some inside RumeGrpcTraceLayer");
                let ext_req_id = req
                    .extensions()
                    .get::<RequestId>()
                    .expect("RequestId must be in extensions")
                    .as_str()
                    .to_string();
                Ok::<_, Infallible>((current_tc.trace_id().to_string(), ext_req_id))
            },
        ));

        let req = axum::http::Request::builder()
            .uri("/rume.v1.Echo/Ping")
            .header("traceparent", tp)
            .header("x-request-id", "grpc-req-7")
            .body(())
            .unwrap();

        let (trace_id, req_id) = svc.oneshot(req).await.unwrap();
        assert_eq!(trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(req_id, "grpc-req-7");
    }
}
