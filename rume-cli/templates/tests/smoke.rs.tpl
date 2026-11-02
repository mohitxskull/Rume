use rume::app::AppContext;
use rume::prelude::*;
use {{crate_name}}::app::{AppState, build_router};
use {{crate_name}}::config::Config;
use {{crate_name}}::env::Env;

#[test]
fn config_loads() {
    // Ensure test-specific config is used.
    unsafe {
        std::env::set_var("RUME_ENV", "test");
    }
    let config: Config = rume::config::load().expect("config should load");
    assert!(!config.app.name.is_empty());
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    unsafe {
        std::env::set_var("RUME_ENV", "test");
    }
    let env = rume::env::load::<Env>().expect("env should load");
    let config = rume::config::load::<Config>().expect("config should load");
    let ctx = AppContext::builder(env, config).state(AppState).build();
    let router = build_router(ctx.clone()).with_state(ctx);
    let app = TestApp::new(router);

    let response = app.get("/health").await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"status\":\"ok\""));
}
