//! Axum router integration for tonic gRPC services.

use axum::Router;
use tonic::service::Routes;

/// Mounts tonic [`Routes`] onto an Axum [`Router`] with [`crate::trace::RumeGrpcTraceLayer`]
/// applied so W3C `traceparent`/`tracestate` and validated `x-request-id` propagate into
/// async gRPC handlers (`P2-07`).
pub fn mount_grpc_routes(router: Router, grpc_routes: Routes) -> Router {
    router.merge(
        grpc_routes
            .into_axum_router()
            .layer(crate::trace::RumeGrpcTraceLayer),
    )
}

/// Extension trait for mounting tonic [`Routes`] onto an Axum [`Router`].
pub trait GrpcRouterExt {
    /// Mounts tonic gRPC [`Routes`] onto `self`.
    fn mount_grpc(self, grpc_routes: Routes) -> Self;
}

impl GrpcRouterExt for Router {
    fn mount_grpc(self, grpc_routes: Routes) -> Self {
        mount_grpc_routes(self, grpc_routes)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use axum::body::Body;
    use axum::http::{Request, Response, StatusCode, Version};
    use tonic::body::Body as TonicBody;
    use tonic::server::NamedService;
    use tower::{Service, ServiceExt};

    use super::*;

    #[derive(Clone)]
    struct TestEchoService;

    impl NamedService for TestEchoService {
        const NAME: &'static str = "test.Echo";
    }

    impl Service<Request<TonicBody>> for TestEchoService {
        type Response = Response<TonicBody>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<TonicBody>) -> Self::Future {
            let req_id = req
                .extensions()
                .get::<rume_foundation::RequestId>()
                .map(|id| id.as_str().to_string())
                .unwrap_or_default();
            Box::pin(async move {
                let has_trace_ctx = rume_observability::TraceContext::current().is_some();
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "0")
                    .header(
                        "x-test-has-trace-ctx",
                        if has_trace_ctx { "1" } else { "0" },
                    )
                    .header("x-test-request-id", req_id)
                    .body(TonicBody::empty())
                    .expect("valid test gRPC response"))
            })
        }
    }

    #[tokio::test]
    async fn mount_grpc_routes_dispatches_http2_post_to_mounted_service() {
        let routes = Routes::new(TestEchoService);
        let app = Router::new().mount_grpc(routes);

        let req = Request::builder()
            .method("POST")
            .uri("/test.Echo/Ping")
            .version(Version::HTTP_2)
            .header("content-type", "application/grpc")
            .body(Body::empty())
            .expect("valid test gRPC request");

        let res = app.oneshot(req).await.expect("router response");
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/grpc")
        );
        assert_eq!(
            res.headers()
                .get("grpc-status")
                .and_then(|v| v.to_str().ok()),
            Some("0")
        );
        assert_eq!(
            res.headers()
                .get("x-test-has-trace-ctx")
                .and_then(|v| v.to_str().ok()),
            Some("1"),
            "mount_grpc_routes must wire RumeGrpcTraceLayer so TraceContext::current() is active"
        );
    }

    #[tokio::test]
    async fn mount_grpc_routes_replaces_invalid_request_id_header() {
        let routes = Routes::new(TestEchoService);
        let app = Router::new().mount_grpc(routes);

        let invalid_id = "bad id with spaces and $pecial chars!";
        let req = Request::builder()
            .method("POST")
            .uri("/test.Echo/Ping")
            .version(Version::HTTP_2)
            .header("content-type", "application/grpc")
            .header("x-request-id", invalid_id)
            .body(Body::empty())
            .expect("valid test gRPC request");

        let res = app.oneshot(req).await.expect("router response");
        assert_eq!(res.status(), StatusCode::OK);
        let propagated = res
            .headers()
            .get("x-test-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_ne!(propagated, invalid_id);
        assert!(
            rume_observability::is_valid_request_id(propagated),
            "expected regenerated valid RequestId, got '{propagated}'"
        );
    }
}
