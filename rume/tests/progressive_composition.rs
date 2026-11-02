//! Integration tests for progressive application composition and first-class modules (ARCH-01).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use rume::module::{Module, ModuleContext};
use rume::prelude::*;
use rume::routing::{HealthCheckResult, health_routes};

async fn body_string(response: axum::response::Response) -> String {
    let bytes = TestApp::body_bytes(response).await;
    String::from_utf8(bytes).expect("body is utf-8")
}

async fn body_json<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = TestApp::body_bytes(response).await;
    serde_json::from_slice(&bytes).expect("body is valid JSON")
}

// ============================================================================
// 1. Minimal Stateless Application
// ============================================================================

#[tokio::test]
async fn minimal_stateless_application() {
    let app = Rume::new()
        .get("/", || async { "Hello, Rume!" })
        .test_app()
        .expect("minimal app should build without error");

    let res = app.get("/").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_string(res).await, "Hello, Rume!");
}

#[tokio::test]
async fn builder_mounts_openapi_and_metrics_endpoints() {
    let registry = rume::metrics::MetricsRegistry::new();
    registry.record_http_request("GET", 200, std::time::Duration::from_millis(5));

    let openapi_info = rume::openapi::OpenApiInfo::new("App API", "1.0.0");

    let app = Rume::new()
        .get("/hello", || async { "world" })
        .openapi("/openapi.json", openapi_info)
        .metrics("/metrics", registry)
        .test_app()
        .expect("build app");

    let res = app.get("/openapi.json").await;
    assert_eq!(res.status(), StatusCode::OK);
    let doc: serde_json::Value = body_json(res).await;
    assert_eq!(doc["info"]["title"], "App API");
    assert!(doc["paths"]["/hello"].is_object());

    let res = app.get("/metrics").await;
    assert_eq!(res.status(), StatusCode::OK);
    let metrics_text = body_string(res).await;
    assert!(metrics_text.contains("http_requests_total"));
}

// ============================================================================
// 2. Stateful Application with Two Modules (Encapsulated Scoped State)
// ============================================================================

#[derive(Clone)]
struct UsersState {
    users: Arc<Mutex<Vec<String>>>,
}

struct UsersModule {
    state: UsersState,
}

impl UsersModule {
    fn new(users: Vec<String>) -> Self {
        Self {
            state: UsersState {
                users: Arc::new(Mutex::new(users)),
            },
        }
    }
}

impl Module for UsersModule {
    fn name(&self) -> &str {
        "users"
    }

    fn register_routes(&self, router: Router) -> Router {
        let users_router = Router::new()
            .get("/users", |State(state): State<UsersState>| async move {
                let list = state.users.lock().unwrap().clone();
                axum::Json(list)
            })
            .with_state(self.state.clone());

        router.merge(users_router)
    }
}

#[derive(Clone)]
struct BillingState {
    balance: Arc<AtomicI64>,
}

struct BillingModule {
    state: BillingState,
}

impl BillingModule {
    fn new(initial_balance: i64) -> Self {
        Self {
            state: BillingState {
                balance: Arc::new(AtomicI64::new(initial_balance)),
            },
        }
    }
}

impl Module for BillingModule {
    fn name(&self) -> &str {
        "billing"
    }

    fn register_routes(&self, router: Router) -> Router {
        let billing_router = Router::new()
            .get(
                "/billing/balance",
                |State(state): State<BillingState>| async move {
                    state.balance.load(Ordering::SeqCst).to_string()
                },
            )
            .with_state(self.state.clone());

        router.merge(billing_router)
    }
}

#[tokio::test]
async fn stateful_application_with_two_modules() {
    let users_mod = UsersModule::new(vec!["alice".to_string(), "bob".to_string()]);
    let billing_mod = BillingModule::new(4200);

    let app = Rume::new()
        .module(users_mod)
        .module(billing_mod)
        .test_app()
        .expect("stateful modular app should build without error");

    // 1. Users module endpoint
    let res = app.get("/users").await;
    assert_eq!(res.status(), StatusCode::OK);
    let users: Vec<String> = body_json(res).await;
    assert_eq!(users, vec!["alice", "bob"]);

    // 2. Billing module endpoint
    let res = app.get("/billing/balance").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_string(res).await, "4200");
}

// ============================================================================
// 3. Explicit Module Dependencies and Topological Startup Ordering
// ============================================================================

struct DepBillingModule {
    events: Arc<Mutex<Vec<String>>>,
}

impl Module for DepBillingModule {
    fn name(&self) -> &str {
        "billing"
    }

    fn dependencies(&self) -> Vec<&str> {
        vec!["users"]
    }

    fn on_start<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> Pin<
        Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push("billing:start".to_string());
            Ok(())
        })
    }
}

struct DepUsersModule {
    events: Arc<Mutex<Vec<String>>>,
}

impl Module for DepUsersModule {
    fn name(&self) -> &str {
        "users"
    }

    fn on_start<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> Pin<
        Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.events.lock().unwrap().push("users:start".to_string());
            Ok(())
        })
    }
}

#[tokio::test]
async fn modules_with_explicit_dependencies_topological_startup() {
    let events = Arc::new(Mutex::new(Vec::new()));

    let billing = DepBillingModule {
        events: Arc::clone(&events),
    };
    let users = DepUsersModule {
        events: Arc::clone(&events),
    };

    // Registered in REVERSE dependency order (billing registered before users)
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let builder = Rume::new().module(billing).module(users);

    let server_task = tokio::spawn(async move {
        builder
            .run_with_listener_and_signal(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    // Wait briefly for startup to complete
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trigger shutdown
    let _ = shutdown_tx.send(());
    server_task
        .await
        .unwrap()
        .expect("server should shut down cleanly");

    // Invariant: "users" must start BEFORE "billing" despite reverse registration order
    let history = events.lock().unwrap().clone();
    assert_eq!(
        history,
        vec!["users:start", "billing:start"],
        "dependency 'users' must start before dependent 'billing'"
    );
}

// ============================================================================
// 4. Invalid Circular Dependency Detection (Fails Closed at Preflight)
// ============================================================================

struct CycleModuleA;
impl Module for CycleModuleA {
    fn name(&self) -> &str {
        "a"
    }
    fn dependencies(&self) -> Vec<&str> {
        vec!["b"]
    }
}

struct CycleModuleB;
impl Module for CycleModuleB {
    fn name(&self) -> &str {
        "b"
    }
    fn dependencies(&self) -> Vec<&str> {
        vec!["a"]
    }
}

#[tokio::test]
async fn circular_dependency_detection_fails_preflight_with_zero_side_effects() {
    let side_effect_ran = Arc::new(AtomicBool::new(false));
    let side_effect_clone = Arc::clone(&side_effect_ran);

    let builder = Rume::new()
        .module(CycleModuleA)
        .module(CycleModuleB)
        .on_ready(move |_| async move {
            side_effect_clone.store(true, Ordering::SeqCst);
        });

    // 1. Test via test_app preflight
    let err = match builder.test_app() {
        Ok(_) => panic!("circular module dependency must fail preflight"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let msg = err.to_string();
    assert!(msg.contains("circular module dependency detected"));
    assert!(msg.contains("a -> b -> a") || msg.contains("b -> a -> b"));

    // 2. Verify zero startup side effects
    assert!(
        !side_effect_ran.load(Ordering::SeqCst),
        "startup hooks must never run when a circular dependency is detected"
    );
}

// ============================================================================
// 5. Independent Module Startup (Topological) and Shutdown (Reverse) with Rollback
// ============================================================================

struct LifecycleModule {
    name: &'static str,
    deps: Vec<&'static str>,
    events: Arc<Mutex<Vec<String>>>,
}

impl Module for LifecycleModule {
    fn name(&self) -> &str {
        self.name
    }

    fn dependencies(&self) -> Vec<&str> {
        self.deps.clone()
    }

    fn on_start<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> Pin<
        Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:start", self.name));
            Ok(())
        })
    }

    fn on_shutdown<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:shutdown", self.name));
        })
    }
}

#[tokio::test]
async fn independent_module_startup_and_reverse_shutdown() {
    let events = Arc::new(Mutex::new(Vec::new()));

    // Graph: core -> auth -> api
    let core = LifecycleModule {
        name: "core",
        deps: vec![],
        events: Arc::clone(&events),
    };
    let auth = LifecycleModule {
        name: "auth",
        deps: vec!["core"],
        events: Arc::clone(&events),
    };
    let api = LifecycleModule {
        name: "api",
        deps: vec!["auth"],
        events: Arc::clone(&events),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    // Register out of order: api, core, auth
    let builder = Rume::new().module(api).module(core).module(auth);

    let server_task = tokio::spawn(async move {
        builder
            .run_with_listener_and_signal(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Shutdown
    let _ = shutdown_tx.send(());
    server_task.await.unwrap().expect("clean shutdown");

    let history = events.lock().unwrap().clone();
    assert_eq!(
        history,
        vec![
            "core:start",
            "auth:start",
            "api:start",
            "api:shutdown",
            "auth:shutdown",
            "core:shutdown",
        ],
        "startup must be topological and shutdown must be reverse topological"
    );
}

struct FailingModule;
impl Module for FailingModule {
    fn name(&self) -> &str {
        "failing"
    }

    fn dependencies(&self) -> Vec<&str> {
        vec!["healthy"]
    }

    fn on_start<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> Pin<
        Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>,
    > {
        Box::pin(async move { Err("simulated startup failure".into()) })
    }
}

#[tokio::test]
async fn module_startup_failure_triggers_rollback_of_already_started_modules() {
    let events = Arc::new(Mutex::new(Vec::new()));

    let healthy = LifecycleModule {
        name: "healthy",
        deps: vec![],
        events: Arc::clone(&events),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let builder = Rume::new().module(healthy).module(FailingModule);

    let result = builder
        .run_with_listener_and_signal(listener, async move {
            let _ = shutdown_rx.await;
        })
        .await;

    assert!(
        result.is_err(),
        "startup must return error on module failure"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("module 'failing' failed to start"));

    let history = events.lock().unwrap().clone();
    assert_eq!(
        history,
        vec!["healthy:start", "healthy:shutdown"],
        "healthy module must be rolled back via on_shutdown when subsequent module fails"
    );
}

// ============================================================================
// 6. Module-Specific Middleware and Health Checks
// ============================================================================

#[derive(Debug)]
struct DatabaseHealthCheck;
impl HealthCheck for DatabaseHealthCheck {
    fn name(&self) -> &str {
        "postgres_pool"
    }

    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
        Box::pin(async {
            HealthCheckResult {
                name: "postgres_pool".to_string(),
                status: HealthStatus::Ok,
                message: "pool healthy".to_string(),
                meta: Default::default(),
                cached: false,
            }
        })
    }
}

struct MonitoredModule;
impl Module for MonitoredModule {
    fn name(&self) -> &str {
        "monitored"
    }

    fn register_health_checks(&self, checks: &mut HealthChecks) {
        checks.add(DatabaseHealthCheck);
    }

    fn register_routes(&self, router: Router) -> Router {
        // Module-specific route with module-specific middleware layer
        let monitored_router = Router::new()
            .get("/monitored/data", || async { "secure data" })
            .layer(axum::middleware::from_fn(
                |req, next: axum::middleware::Next| async move {
                    let mut res = next.run(req).await;
                    res.headers_mut().insert(
                        axum::http::HeaderName::from_static("x-module-origin"),
                        axum::http::HeaderValue::from_static("monitored"),
                    );
                    res
                },
            ));

        router.merge(monitored_router)
    }
}

#[tokio::test]
async fn module_specific_middleware_and_health_checks() {
    let app = Rume::new()
        .merge(health_routes())
        .module(MonitoredModule)
        .test_app()
        .expect("monitored module app should build");

    // 1. Verify module-specific middleware executed
    let res = app.get("/monitored/data").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("x-module-origin")
            .and_then(|v| v.to_str().ok()),
        Some("monitored")
    );
    assert_eq!(body_string(res).await, "secure data");

    // 2. Verify health endpoint reflects registered check
    let res = app.get("/health/ready").await;
    assert_eq!(res.status(), StatusCode::OK);
}

// ============================================================================
// 7. Custom Axum Services, Fallback, and Tower Layers
// ============================================================================

#[tokio::test]
async fn custom_services_fallback_and_tower_layers() {
    let app = Rume::new()
        .get("/hello", || async { "world" })
        // 1. Custom Fallback handler
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                [("content-type", "application/json")],
                r#"{"error":"custom_not_found"}"#,
            )
                .into_response()
        })
        // 2. Custom Tower Layer applied to the application (wraps both routes and fallback)
        .layer(axum::middleware::from_fn(
            |req, next: axum::middleware::Next| async move {
                let mut res = next.run(req).await;
                res.headers_mut().insert(
                    axum::http::HeaderName::from_static("x-custom-layer"),
                    axum::http::HeaderValue::from_static("active"),
                );
                res
            },
        ))
        .test_app()
        .expect("custom composition should build");

    // Request to known route
    let res = app.get("/hello").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("x-custom-layer")
            .and_then(|v| v.to_str().ok()),
        Some("active")
    );
    assert_eq!(body_string(res).await, "world");

    // Request to unknown route triggers custom fallback
    let res = app.get("/nonexistent/endpoint").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()
            .get("x-custom-layer")
            .and_then(|v| v.to_str().ok()),
        Some("active")
    );
    assert_eq!(body_string(res).await, r#"{"error":"custom_not_found"}"#);
}
