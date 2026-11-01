use axum::{Json, Router, routing::get};
use rume::testing::TestApp;
use serde::Serialize;

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let router = Router::new().route("/health", get(health));
    let app = TestApp::new(router);

    let response = app.get("/health").await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert_eq!(body, r#"{"status":"ok"}"#);
}
