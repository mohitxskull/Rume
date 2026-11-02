use rume::config;
use rume::prelude::*;

#[derive(Deserialize)]
struct Config {
    app: AppConfig,
}

#[derive(Deserialize)]
struct AppConfig {
    name: String,
}

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
    use axum::{Json, Router, routing::get};

    #[derive(Serialize)]
    struct Health {
        status: &'static str,
    }

    async fn health() -> Json<Health> {
        Json(Health { status: "ok" })
    }

    let router = Router::new().route("/health", get(health));
    let app = TestApp::new(router);

    let response = app.get("/health").await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert_eq!(body, r#"{"status":"ok"}"#);
}
