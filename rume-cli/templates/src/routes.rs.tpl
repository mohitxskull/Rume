use rume::axum::Router;

pub fn router() -> Router<()> {
    let mut router = Router::new();
    router = router.route("/health", rume::axum::routing::get(crate::handlers::health::health));
    // rume:routes
    router
}
