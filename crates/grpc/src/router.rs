//! Axum router integration for tonic gRPC services.

use axum::Router;
use tonic::service::Routes;

/// Mounts tonic [`Routes`] onto an Axum [`Router`].
pub fn mount_grpc_routes(router: Router, grpc_routes: Routes) -> Router {
    router.merge(grpc_routes.into_axum_router())
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
