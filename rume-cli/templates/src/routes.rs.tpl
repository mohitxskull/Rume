use rume::axum::Router;

pub fn router() -> Router<()> {
    Router::new().route("/health", rume::axum::routing::get(crate::handlers::health::health))
}
