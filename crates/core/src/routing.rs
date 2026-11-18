//! Routing re-exports from `rume-http` with `AppContext`-typed `health_routes`.

pub use rume_http::routing::*;

/// Routes for health checks (`/health/live` and `/health/ready`) bound to [`crate::app::AppContext`].
pub fn health_routes<E, C, S>() -> Router<crate::app::AppContext<E, C, S>>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    rume_http::routing::health_routes::<crate::app::AppContext<E, C, S>>()
}
