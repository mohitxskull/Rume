//! Integration tests for real `health_routes` with `AppContext` state.
//! T-T8: exercises `health_routes`, availability transitions, and
//! `HealthChecks::run` against an actual router (not a bare handler).

use rume::app::{AppContext, Availability, ProjectPaths};
use rume::axum::http::StatusCode;
use rume::prelude::{TestApp, TestAppBuilder};
use rume::routing::{HealthCheck, HealthCheckResult, HealthChecks, HealthStatus, health_routes};
use std::collections::BTreeMap;
use std::pin::Pin;

/// A check that always reports Ok.
#[derive(Debug)]
struct OkCheck;

impl HealthCheck for OkCheck {
    fn name(&self) -> &str {
        "database"
    }

    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
        Box::pin(async move {
            HealthCheckResult {
                name: "database".to_string(),
                status: HealthStatus::Ok,
                message: "connection established".to_string(),
                meta: BTreeMap::new(),
                cached: false,
            }
        })
    }
}

/// A check that always reports Warning.
#[derive(Debug)]
struct WarningCheck;

impl HealthCheck for WarningCheck {
    fn name(&self) -> &str {
        "cache"
    }

    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
        Box::pin(async move {
            HealthCheckResult {
                name: "cache".to_string(),
                status: HealthStatus::Warning,
                message: "high latency".to_string(),
                meta: BTreeMap::new(),
                cached: false,
            }
        })
    }
}

/// A check that always reports Failed.
#[derive(Debug)]
struct FailedCheck;

impl HealthCheck for FailedCheck {
    fn name(&self) -> &str {
        "s3"
    }

    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
        Box::pin(async move {
            HealthCheckResult {
                name: "s3".to_string(),
                status: HealthStatus::Failed,
                message: "credentials invalid".to_string(),
                meta: BTreeMap::new(),
                cached: false,
            }
        })
    }
}

fn build_ctx() -> AppContext<std::net::IpAddr, std::collections::BTreeMap<String, String>> {
    AppContext::new(
        std::net::IpAddr::from([127, 0, 0, 1]),
        std::collections::BTreeMap::<String, String>::new(),
        ProjectPaths::from_current_dir(),
        Availability::new(),
        HealthChecks::new(),
    )
}

#[tokio::test]
async fn health_live_returns_ok_when_not_broken() {
    let ctx = build_ctx();
    let app = TestAppBuilder::new(ctx.clone())
        .router(health_routes())
        .build()
        .await;

    let response = app.get("/health/live").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"status\":\"alive\""));
}

#[tokio::test]
async fn health_ready_returns_ok_when_all_checks_ok() {
    let ctx = AppContext::builder(
        std::net::IpAddr::from([127, 0, 0, 1]),
        std::collections::BTreeMap::<String, String>::new(),
    )
    .health_checks(HealthChecks::new().register(OkCheck))
    .build();

    let app = TestAppBuilder::new(ctx.clone())
        .router(health_routes())
        .build()
        .await;

    let response = app.get("/health/ready").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"status\":\"ok\""));
    assert!(body.contains("\"name\":\"database\""));
    assert!(!body.contains("\"pid\""));
    assert!(!body.contains("\"platform\""));
}

#[tokio::test]
async fn health_ready_returns_503_on_warning() {
    // C-m4: Warning status triggers 503 — readiness requires all Ok.
    let ctx = AppContext::builder(
        std::net::IpAddr::from([127, 0, 0, 1]),
        std::collections::BTreeMap::<String, String>::new(),
    )
    .health_checks(HealthChecks::new().register(WarningCheck))
    .build();

    let app = TestAppBuilder::new(ctx.clone())
        .router(health_routes())
        .build()
        .await;

    let response = app.get("/health/ready").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"status\":\"warning\""));
}

#[tokio::test]
async fn health_ready_returns_503_on_failed() {
    let ctx = AppContext::builder(
        std::net::IpAddr::from([127, 0, 0, 1]),
        std::collections::BTreeMap::<String, String>::new(),
    )
    .health_checks(HealthChecks::new().register(FailedCheck))
    .build();

    let app = TestAppBuilder::new(ctx.clone())
        .router(health_routes())
        .build()
        .await;

    let response = app.get("/health/ready").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = TestApp::body_text(response).await;
    assert!(body.contains("\"status\":\"error\""));
}

#[tokio::test]
async fn health_ready_excludes_debug_info() {
    // S-m2: pid/platform must not appear in /health/ready response.
    let ctx = AppContext::builder(
        std::net::IpAddr::from([127, 0, 0, 1]),
        std::collections::BTreeMap::<String, String>::new(),
    )
    .health_checks(HealthChecks::new().register(OkCheck))
    .build();

    let app = TestAppBuilder::new(ctx.clone())
        .router(health_routes())
        .build()
        .await;

    let response = app.get("/health/ready").await;
    let body = TestApp::body_text(response).await;
    assert!(!body.contains("pid"));
    assert!(!body.contains("platform"));
}
