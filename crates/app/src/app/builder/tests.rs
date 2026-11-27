use super::*;
use crate::app::builder::router::normalize_route_template;
use crate::app::context::AppContext;
use crate::app::paths::ProjectPaths;
use crate::app::state::{ApplicationMode, Availability, AvailabilityState, LifecycleState};
use rume_http::routing::HealthChecks;

fn test_ctx() -> AppContext<(), (), ()> {
    AppContext::new(
        (),
        (),
        ProjectPaths::from_current_dir(),
        Availability::new(),
        HealthChecks::new(),
        (),
    )
}

/// Returns a boxed middleware closure that adds a route at `path`.
#[allow(clippy::type_complexity)]
fn mw_add_route(
    path: &'static str,
) -> Box<
    dyn Fn(Router<AppContext<(), (), ()>>) -> Router<AppContext<(), (), ()>>
        + Send
        + Sync
        + 'static,
> {
    Box::new(move |router: Router<AppContext<(), (), ()>>| {
        router.route(path, axum::routing::get(|| async {}))
    })
}

#[test]
fn known_middleware_applied_successfully() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .middleware("auth", mw_add_route("/auth"))
        .use_middleware("auth");

    let router = builder.build_router(test_ctx());
    assert!(router.is_ok());
}

#[test]
fn unknown_middleware_via_use_middleware_is_rejected() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx).use_middleware("auth");

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::Middleware(MiddlewareConfigError::UnknownMiddleware { name })
        if name == "auth"
    ));
}

#[test]
fn duplicate_middleware_registration_is_rejected() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .middleware("auth", mw_add_route("/auth"))
        .middleware("auth", mw_add_route("/auth2"));

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::Middleware(MiddlewareConfigError::DuplicateMiddleware { name })
        if name == "auth"
    ));
}

#[test]
fn unknown_group_via_use_group_is_rejected() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx).use_group("admins");

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::Middleware(MiddlewareConfigError::UnknownGroup { name })
        if name == "admins"
    ));
}

#[test]
fn group_with_unknown_member_is_rejected() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .middleware("auth", mw_add_route("/auth"))
        .group("admins", &["auth", "missing"])
        .use_group("admins");

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::Middleware(MiddlewareConfigError::UnknownGroupMember { group, member })
        if group == "admins" && member == "missing"
    ));
}

#[test]
fn duplicate_group_definition_is_rejected() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .group("admins", &[])
        .group("admins", &[])
        .use_group("admins");

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::Middleware(MiddlewareConfigError::DuplicateGroup { name })
        if name == "admins"
    ));
}

#[test]
fn middleware_applied_in_registration_order() {
    // Named middleware wraps the router in registration order. We can't
    // observe request-time ordering without a live HTTP client, but we can
    // verify that all applied middleware are present and the result is Ok.
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .middleware("first", mw_add_route("/first"))
        .middleware("second", mw_add_route("/second"))
        .use_middleware("first")
        .use_middleware("second");

    let router = builder.build_router(test_ctx());
    assert!(router.is_ok());
}

#[test]
fn group_members_applied_in_declaration_order() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .middleware("a", mw_add_route("/a"))
        .middleware("b", mw_add_route("/b"))
        .middleware("c", mw_add_route("/c"))
        .group("stack", &["a", "b", "c"])
        .use_group("stack");

    let router = builder.build_router(test_ctx());
    assert!(router.is_ok());
}

#[test]
fn run_returns_error_on_misconfigured_middleware() {
    // `run()` fails closed: unknown middleware prevents serving.
    // `run()` calls `build_router` before listening, so the error surfaces
    // before any traffic can be served.
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx).use_middleware("nonexistent");

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::Middleware(MiddlewareConfigError::UnknownMiddleware { .. })
    ));
}

#[test]
fn error_display_messages_are_descriptive() {
    let err = MiddlewareConfigError::UnknownMiddleware {
        name: "autn".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("autn"));
    assert!(msg.contains(".middleware()"));

    let err = MiddlewareConfigError::UnknownGroupMember {
        group: "admins".to_string(),
        member: "auth".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("admins"));
    assert!(msg.contains("auth"));
}

#[tokio::test]
async fn inspect_mode_does_not_run_on_ready_hooks() {
    // Inspect mode must not run startup hooks — they may have side effects.
    let ready_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_ran_clone = ready_ran.clone();

    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .mode(ApplicationMode::Inspect)
        .on_ready(move |_| async move {
            ready_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

    builder.run().await.expect("inspect mode should succeed");

    assert!(
        !ready_ran.load(std::sync::atomic::Ordering::SeqCst),
        "on_ready hooks must not run in Inspect mode"
    );
}

#[tokio::test]
async fn inspect_mode_does_not_run_on_shutdown_hooks() {
    // Inspect mode must not run shutdown hooks either.
    let shutdown_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_ran_clone = shutdown_ran.clone();

    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .mode(ApplicationMode::Inspect)
        .on_shutdown(move |_| async move {
            shutdown_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

    builder.run().await.expect("inspect mode should succeed");

    assert!(
        !shutdown_ran.load(std::sync::atomic::Ordering::SeqCst),
        "on_shutdown hooks must not run in Inspect mode"
    );
}

#[tokio::test]
async fn inspect_mode_sets_booted_live_then_returns() {
    // Inspect mode boots to Booted/Live and returns without serving.
    let ctx = test_ctx();
    let lifecycle = ctx.lifecycle.clone();
    let availability = ctx.availability.clone();

    let builder = RumeBuilder::new(ctx).mode(ApplicationMode::Inspect);

    builder.run().await.expect("inspect mode should succeed");

    // Lifecycle should be Booted (not Ready — not serving traffic).
    assert_eq!(lifecycle.state(), LifecycleState::Booted);
    assert!(availability.state() == AvailabilityState::Live);
}

#[tokio::test]
async fn preflight_failure_aborts_before_startup_hooks_run() {
    // Preflight validation must fail closed BEFORE any startup hooks run.
    let ready_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_ran_clone = ready_ran.clone();

    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .use_middleware("nonexistent")
        .on_ready(move |_| async move {
            ready_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

    let err = builder.run().await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        !ready_ran.load(std::sync::atomic::Ordering::SeqCst),
        "on_ready hook must NOT run when preflight validation fails"
    );
}

#[tokio::test]
async fn fallible_startup_hook_failure_triggers_cleanup_hooks() {
    // If an on_ready hook fails, registered shutdown hooks must be executed
    // to clean up already-acquired resources.
    let cleanup_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cleanup_ran_clone = cleanup_ran.clone();

    let ctx = test_ctx();
    let _availability = ctx.availability.clone();
    let lifecycle = ctx.lifecycle.clone();

    let builder = RumeBuilder::new(ctx)
        .bind("127.0.0.1:0")
        .on_ready(|_| async move {
            Err::<(), _>(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "database unreachable",
            ))
        })
        .on_shutdown(move |_| async move {
            cleanup_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

    let err = builder.run().await.unwrap_err();
    assert!(err.to_string().contains("database unreachable"));

    assert!(
        cleanup_ran.load(std::sync::atomic::Ordering::SeqCst),
        "on_shutdown cleanup hooks must run when a startup hook fails"
    );
    assert_eq!(lifecycle.state(), LifecycleState::Terminated);
}

type TestMiddleware = Box<
    dyn Fn(Router<AppContext<(), (), ()>>) -> Router<AppContext<(), (), ()>>
        + Send
        + Sync
        + 'static,
>;

fn recording_mw(
    name: &'static str,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> TestMiddleware {
    Box::new(move |router| {
        let log = std::sync::Arc::clone(&log);
        router.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let log = std::sync::Arc::clone(&log);
                async move {
                    log.lock().unwrap().push(format!("{name}:enter"));
                    let res = next.run(req).await;
                    log.lock().unwrap().push(format!("{name}:exit"));
                    res
                }
            },
        ))
    })
}

#[tokio::test]
async fn middleware_individual_execution_order_during_request() {
    use tower::ServiceExt;

    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = test_ctx();

    let builder = RumeBuilder::new(ctx)
        .route("/hello", axum::routing::get(|| async { "ok" }))
        .middleware("first", recording_mw("first", std::sync::Arc::clone(&log)))
        .middleware(
            "second",
            recording_mw("second", std::sync::Arc::clone(&log)),
        )
        .use_middleware("first")
        .use_middleware("second");

    let app_router = builder.build_router(test_ctx()).unwrap();
    let app = app_router.with_state(test_ctx());

    let req = axum::http::Request::builder()
        .uri("/hello")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let events = log.lock().unwrap().clone();
    assert_eq!(
        events,
        vec!["first:enter", "second:enter", "second:exit", "first:exit"],
        "individual middleware must enter in registration order and exit in reverse"
    );
}

#[tokio::test]
async fn middleware_group_execution_order_during_request() {
    use tower::ServiceExt;

    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = test_ctx();

    let builder = RumeBuilder::new(ctx)
        .route("/hello", axum::routing::get(|| async { "ok" }))
        .middleware("a", recording_mw("a", std::sync::Arc::clone(&log)))
        .middleware("b", recording_mw("b", std::sync::Arc::clone(&log)))
        .middleware("c", recording_mw("c", std::sync::Arc::clone(&log)))
        .group("stack", &["a", "b", "c"])
        .use_group("stack");

    let app_router = builder.build_router(test_ctx()).unwrap();
    let app = app_router.with_state(test_ctx());

    let req = axum::http::Request::builder()
        .uri("/hello")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let events = log.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "a:enter", "b:enter", "c:enter", "c:exit", "b:exit", "a:exit"
        ],
        "group members must enter in declaration order and exit in reverse"
    );
}

#[tokio::test]
async fn middleware_combined_individual_and_groups_execution_order() {
    use tower::ServiceExt;

    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = test_ctx();

    let builder = RumeBuilder::new(ctx)
        .route("/hello", axum::routing::get(|| async { "ok" }))
        .middleware(
            "global_mw",
            recording_mw("global_mw", std::sync::Arc::clone(&log)),
        )
        .middleware("auth", recording_mw("auth", std::sync::Arc::clone(&log)))
        .middleware("audit", recording_mw("audit", std::sync::Arc::clone(&log)))
        .group("api", &["auth", "audit"])
        .use_middleware("global_mw")
        .use_group("api");

    let app_router = builder.build_router(test_ctx()).unwrap();
    let app = app_router.with_state(test_ctx());

    let req = axum::http::Request::builder()
        .uri("/hello")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);

    let events = log.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "global_mw:enter",
            "auth:enter",
            "audit:enter",
            "audit:exit",
            "auth:exit",
            "global_mw:exit"
        ],
        "individual middleware must wrap outside groups: global -> group -> handler -> group -> global"
    );
}

#[tokio::test]
async fn app_builder_topological_startup_and_reverse_shutdown_on_failure() {
    struct RecordingModule {
        name: &'static str,
        deps: Vec<&'static str>,
        fail_start: bool,
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl crate::module::Module for RecordingModule {
        fn name(&self) -> &str {
            self.name
        }

        fn dependencies(&self) -> Vec<&str> {
            self.deps.clone()
        }

        fn on_start<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, crate::module::ModuleHookResult> {
            Box::pin(async move {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("start:{}", self.name));
                if self.fail_start {
                    Err("boom".into())
                } else {
                    Ok(())
                }
            })
        }

        fn on_shutdown<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, ()> {
            Box::pin(async move {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("shutdown:{}", self.name));
            })
        }
    }

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .bind("127.0.0.1:0")
        .module(RecordingModule {
            name: "c",
            deps: vec!["b"],
            fail_start: true,
            events: std::sync::Arc::clone(&events),
        })
        .module(RecordingModule {
            name: "b",
            deps: vec!["a"],
            fail_start: false,
            events: std::sync::Arc::clone(&events),
        })
        .module(RecordingModule {
            name: "a",
            deps: vec![],
            fail_start: false,
            events: std::sync::Arc::clone(&events),
        });

    let err = builder
        .run_with_signal(std::future::pending())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("c"));
    assert_eq!(
        *events.lock().unwrap(),
        vec!["start:a", "start:b", "start:c", "shutdown:b", "shutdown:a",]
    );
}

/// A test module that mounts a single GET route.
struct MountRouteModule;
impl crate::module::Module for MountRouteModule {
    fn name(&self) -> &'static str {
        "mount_route"
    }
    fn register_routes(&self, router: rume_http::routing::Router) -> rume_http::routing::Router {
        router.get("/m/x", || async { "module" })
    }
}

#[test]
fn module_route_conflicting_with_app_route_is_rejected() {
    // Module routes must be registered before `route_registry` is read into
    // `seen_routes`, so they participate in duplicate-route detection.
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .get("/m/x", || async { "app" })
        .module(MountRouteModule);

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(err, BuildError::DuplicateRoute { path, .. } if path == "/m/x"));
}

#[test]
fn conflicting_capture_parameter_names_are_rejected_as_duplicate_route() {
    struct CaptureRouteModule;
    impl crate::module::Module for CaptureRouteModule {
        fn name(&self) -> &'static str {
            "capture_route"
        }
        fn register_routes(
            &self,
            router: rume_http::routing::Router,
        ) -> rume_http::routing::Router {
            router.get("/users/{id}", || async { "module" })
        }
    }

    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .get("/users/{user_id}", || async { "app" })
        .module(CaptureRouteModule);

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::DuplicateRoute { method, path }
        if method == "GET" && path == "/users/{user_id}"
    ));
}

#[test]
fn normalize_route_template_normalizes_captures_and_wildcards() {
    assert_eq!(normalize_route_template("/users/{id}"), "/users/{}");
    assert_eq!(normalize_route_template("/users/{user_id}"), "/users/{}");
    assert_eq!(normalize_route_template("/users/:id"), "/users/{}");
    assert_eq!(normalize_route_template("/files/{*rest}"), "/files/{*}");
    assert_eq!(normalize_route_template("/users/list"), "/users/list");
}

#[test]
fn disjoint_methods_on_identical_template_are_allowed() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .get("/users/{id}", || async { "get" })
        .post("/users/{id}", || async { "post" });

    assert!(builder.build_router(test_ctx()).is_ok());
}

#[test]
fn opaque_method_router_with_disjoint_method_is_allowed() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .route("/items", axum::routing::get(|| async { "get" }))
        .post("/items", || async { "post" });

    assert!(builder.build_router(test_ctx()).is_ok());
}

#[test]
fn any_route_still_conflicts_with_specific_method_on_same_path() {
    let ctx = test_ctx();
    let builder = RumeBuilder::new(ctx)
        .any("/items", || async { "any" })
        .post("/items", || async { "post" });

    let err = builder.build_router(test_ctx()).unwrap_err();
    assert!(matches!(
        err,
        BuildError::DuplicateRoute { method, path }
        if method == "POST" && path == "/items"
    ));
}

#[tokio::test]
async fn into_test_parts_and_on_ready_observe_module_routes_and_health_checks() {
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ModDbHealthCheck;

    impl rume_http::routing::HealthCheck for ModDbHealthCheck {
        fn name(&self) -> &str {
            "mod_db"
        }

        fn run(
            &self,
        ) -> std::pin::Pin<Box<dyn Future<Output = rume_http::routing::HealthCheckResult> + Send>>
        {
            Box::pin(async {
                rume_http::routing::HealthCheckResult {
                    name: "mod_db".to_string(),
                    status: rume_http::routing::HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                    duration_ms: None,
                }
            })
        }
    }

    struct ObservedModule;

    impl crate::module::Module for ObservedModule {
        fn name(&self) -> &str {
            "observed"
        }

        fn register_routes(
            &self,
            router: rume_http::routing::Router,
        ) -> rume_http::routing::Router {
            router.get("/mod-items", || async { "mod-items" })
        }

        fn register_health_checks(&self, checks: &mut rume_http::routing::HealthChecks) {
            checks.add(ModDbHealthCheck);
        }
    }

    // 1. `into_test_parts` must expose module routes & health checks on `TestAppParts.ctx`.
    let parts = RumeBuilder::new(test_ctx())
        .module(ObservedModule)
        .into_test_parts()
        .expect("into_test_parts should succeed");
    assert!(
        parts
            .ctx
            .route_registry
            .routes()
            .iter()
            .any(|r| r.path == "/mod-items"),
        "TestAppParts.ctx.route_registry must contain module route /mod-items"
    );
    let report = parts.ctx.health_checks.run().await;
    assert!(
        report.checks.iter().any(|c| c.name == "mod_db"),
        "TestAppParts.ctx.health_checks must contain module health check mod_db"
    );

    // 2. `run_with_signal` must pass finalized context to `on_ready` and `on_shutdown`.
    let ready_observed = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let shutdown_observed = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let ready_spy = Arc::clone(&ready_observed);
    let shutdown_spy = Arc::clone(&shutdown_observed);

    let ctx = test_ctx();
    let lifecycle = ctx.lifecycle.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let mut server = tokio::spawn(async move {
        RumeBuilder::new(ctx)
            .bind("127.0.0.1:0")
            .module(ObservedModule)
            .on_ready(move |ctx| {
                let ready_spy = Arc::clone(&ready_spy);
                async move {
                    let routes: Vec<String> = ctx
                        .route_registry
                        .routes()
                        .iter()
                        .map(|r| r.path.clone())
                        .collect();
                    let checks: Vec<String> = ctx
                        .health_checks
                        .run()
                        .await
                        .checks
                        .into_iter()
                        .map(|c| c.name)
                        .collect();
                    *ready_spy.lock().unwrap() = (routes, checks);
                }
            })
            .on_shutdown(move |ctx| {
                let shutdown_spy = Arc::clone(&shutdown_spy);
                async move {
                    let routes: Vec<String> = ctx
                        .route_registry
                        .routes()
                        .iter()
                        .map(|r| r.path.clone())
                        .collect();
                    let checks: Vec<String> = ctx
                        .health_checks
                        .run()
                        .await
                        .checks
                        .into_iter()
                        .map(|c| c.name)
                        .collect();
                    *shutdown_spy.lock().unwrap() = (routes, checks);
                }
            })
            .run_with_signal(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;
    let _ = shutdown_tx.send(());
    server
        .await
        .expect("server task should join")
        .expect("run_with_signal should exit cleanly");

    let (ready_routes, ready_checks) = ready_observed.lock().unwrap().clone();
    assert!(
        ready_routes.iter().any(|p| p == "/mod-items"),
        "on_ready must observe /mod-items in route_registry, got {ready_routes:?}"
    );
    assert!(
        ready_checks.iter().any(|c| c == "mod_db"),
        "on_ready must observe mod_db in health_checks, got {ready_checks:?}"
    );

    let (shutdown_routes, shutdown_checks) = shutdown_observed.lock().unwrap().clone();
    assert!(
        shutdown_routes.iter().any(|p| p == "/mod-items"),
        "on_shutdown must observe /mod-items in route_registry, got {shutdown_routes:?}"
    );
    assert!(
        shutdown_checks.iter().any(|c| c == "mod_db"),
        "on_shutdown must observe mod_db in health_checks, got {shutdown_checks:?}"
    );

    // 3. `run_with_listener_and_signal` must also pass finalized context to `on_ready` and `on_shutdown`.
    let listener_ready = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let listener_ready_spy = Arc::clone(&listener_ready);
    let ctx = test_ctx();
    let lifecycle = ctx.lifecycle.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let mut server = tokio::spawn(async move {
        RumeBuilder::new(ctx)
            .module(ObservedModule)
            .on_ready(move |ctx| {
                let listener_ready_spy = Arc::clone(&listener_ready_spy);
                async move {
                    let routes: Vec<String> = ctx
                        .route_registry
                        .routes()
                        .iter()
                        .map(|r| r.path.clone())
                        .collect();
                    let checks: Vec<String> = ctx
                        .health_checks
                        .run()
                        .await
                        .checks
                        .into_iter()
                        .map(|c| c.name)
                        .collect();
                    *listener_ready_spy.lock().unwrap() = (routes, checks);
                }
            })
            .run_with_listener_and_signal(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;
    let _ = shutdown_tx.send(());
    server
        .await
        .expect("listener server task should join")
        .expect("run_with_listener_and_signal should exit cleanly");

    let (routes, checks) = listener_ready.lock().unwrap().clone();
    assert!(routes.iter().any(|p| p == "/mod-items"));
    assert!(checks.iter().any(|c| c == "mod_db"));
}

#[tokio::test]
async fn inspect_mode_rejects_module_cycle_and_duplicate_routes() {
    struct CycleMod {
        name: &'static str,
        deps: Vec<&'static str>,
    }

    impl crate::module::Module for CycleMod {
        fn name(&self) -> &str {
            self.name
        }

        fn dependencies(&self) -> Vec<&str> {
            self.deps.clone()
        }
    }

    struct DupRouteMod;

    impl crate::module::Module for DupRouteMod {
        fn name(&self) -> &str {
            "dup_route"
        }

        fn register_routes(
            &self,
            router: rume_http::routing::Router,
        ) -> rume_http::routing::Router {
            router.get("/dup", || async { "module" })
        }
    }

    // (a) Cyclic module dependencies must fail closed with InvalidInput in Inspect mode.
    let cycle_err = RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .module(CycleMod {
            name: "a",
            deps: vec!["b"],
        })
        .module(CycleMod {
            name: "b",
            deps: vec!["a"],
        })
        .run()
        .await
        .expect_err("inspect mode must reject cyclic module dependencies");
    assert_eq!(cycle_err.kind(), std::io::ErrorKind::InvalidInput);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_cycle_err = RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .module(CycleMod {
            name: "a",
            deps: vec!["b"],
        })
        .module(CycleMod {
            name: "b",
            deps: vec!["a"],
        })
        .run_with_listener_and_signal(listener, std::future::pending())
        .await
        .expect_err("listener inspect mode must reject cyclic module dependencies");
    assert_eq!(listener_cycle_err.kind(), std::io::ErrorKind::InvalidInput);

    // (b) Colliding app/module routes must fail closed with InvalidInput in Inspect mode.
    let dup_err = RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .get("/dup", || async { "app" })
        .module(DupRouteMod)
        .run()
        .await
        .expect_err("inspect mode must reject duplicate routes");
    assert_eq!(dup_err.kind(), std::io::ErrorKind::InvalidInput);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_dup_err = RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .get("/dup", || async { "app" })
        .module(DupRouteMod)
        .run_with_listener_and_signal(listener, std::future::pending())
        .await
        .expect_err("listener inspect mode must reject duplicate routes");
    assert_eq!(listener_dup_err.kind(), std::io::ErrorKind::InvalidInput);

    // (c) App `.route("/dup", get(...))` (UNKNOWN metadata) colliding with module
    // `.get("/dup", ...)` must fail cleanly in both Inspect and Run/build_router,
    // while `.route("/dup", post(...))` + module `.get("/dup", ...)` remains valid.
    let unknown_dup_inspect = RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .route("/dup", axum::routing::get(|| async { "app" }))
        .module(DupRouteMod)
        .run()
        .await
        .expect_err("inspect mode must reject app .route(get) + module .get collision");
    assert_eq!(unknown_dup_inspect.kind(), std::io::ErrorKind::InvalidInput);

    let ctx = test_ctx();
    let unknown_dup_build = RumeBuilder::new(ctx.clone())
        .route("/dup", axum::routing::get(|| async { "app" }))
        .module(DupRouteMod)
        .build_router(ctx)
        .expect_err("build_router must return DuplicateRoute instead of panicking");
    assert!(matches!(
        unknown_dup_build,
        BuildError::DuplicateRoute { .. }
    ));

    RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .route("/dup", axum::routing::post(|| async { "app-post" }))
        .module(DupRouteMod)
        .run()
        .await
        .expect("POST /dup and GET /dup on the same path must remain valid in Inspect");
}

async fn wait_for_ready(
    lifecycle: &crate::app::state::Lifecycle,
    server: &mut tokio::task::JoinHandle<Result<(), std::io::Error>>,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if lifecycle.state() == LifecycleState::Ready {
            return;
        }
        if server.is_finished() {
            let res = server.await;
            panic!(
                "server task exited early before reaching Ready (state={:?}): {res:?}",
                lifecycle.state()
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for server to reach Ready (state={:?})",
            lifecycle.state()
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn inspect_mode_fails_closed_on_invalid_cors_config() {
    let invalid_cors = rume_http::transport::CorsConfig::new()
        .allow_any_origin()
        .allow_credentials(true);

    let err = RumeBuilder::new(test_ctx())
        .mode(ApplicationMode::Inspect)
        .cors(invalid_cors)
        .get("/ping", || async { "pong" })
        .run()
        .await
        .expect_err("inspect mode must fail closed on invalid CORS config");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("allow_credentials"),
        "expected CORS error message, got: {err}"
    );
}

#[cfg(feature = "tls")]
#[tokio::test]
async fn rume_builder_tls_serves_https_and_drains_gracefully_on_shutdown() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TEST_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----
MIIBwDCCAWWgAwIBAgIUIi3+ZI/tJFK4wTVlTFPD6QkG8XAwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTEyNjEyMTYwMloYDzIxMDAxMDI5
MTIxNjAyWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAATIw5glQvkynffHCd5iEclrqe+juD6xFx8A1F43+PqLjpra4wZ3non1
Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrryo4GSMIGPMB0GA1UdDgQWBBQQ8rRAB+Bs
+SsH4WcR+cJEbH+hTzAfBgNVHSMEGDAWgBQQ8rRAB+Bs+SsH4WcR+cJEbH+hTzAM
BgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDATBgNVHSUEDDAKBggrBgEFBQcD
ATAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwCgYIKoZIzj0EAwIDSQAwRgIh
APyYGMmbXQV05SjOkoWARBTijDUQ9v3rvdTvfFFJ8HXSAiEA/rJYP8/AcQalmlJL
MOAtrICgSzS3UyLAhl16728zqGc=
-----END CERTIFICATE-----
";

    const TEST_KEY_PEM: &[u8] = b"\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgo2otzH6WgCrTvKb8
Mad8NskKE+yg2Tqk/S0mX41X1AGhRANCAATIw5glQvkynffHCd5iEclrqe+juD6x
Fx8A1F43+PqLjpra4wZ3non1Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrry
-----END PRIVATE KEY-----
";

    let tls_config = rume_http::tls::TlsConfig::from_pem_bytes(TEST_CERT_PEM, TEST_KEY_PEM)
        .expect("valid PEM cert and key");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_hook_ran = Arc::new(AtomicBool::new(false));
    let hook_flag = shutdown_hook_ran.clone();

    let app = Rume::new()
        .tls(tls_config)
        .get(
            "/tls-ping",
            |axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>| async move {
                format!("pong:{peer}")
            },
        )
        .on_shutdown(move |ctx| async move {
            assert_eq!(ctx.lifecycle.state(), LifecycleState::Terminating);
            assert_eq!(ctx.availability.state(), AvailabilityState::RefusingTraffic);
            hook_flag.store(true, Ordering::SeqCst);
        });

    let lifecycle = app.lifecycle();
    let availability = app.availability();

    let mut server = tokio::spawn(async move {
        app.run_with_listener_and_signal(listener, async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;

    let mut root_store = rustls::RootCertStore::empty();
    let mut cert_reader = std::io::BufReader::new(TEST_CERT_PEM);
    for cert in rustls_pemfile::certs(&mut cert_reader) {
        root_store.add(cert.unwrap()).unwrap();
    }
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let client_cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut tls_stream = connector.connect(server_name, tcp).await.unwrap();
    tls_stream
        .write_all(b"GET /tls-ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    tls_stream.read_to_string(&mut response).await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected HTTP/1.1 200 OK over TLS, got: {response}"
    );
    assert!(
        response.contains("pong:127.0.0.1:"),
        "expected ConnectInfo peer address in body, got: {response}"
    );

    let _ = shutdown_tx.send(());
    let server_res = server.await.expect("server task panicked");
    assert!(
        server_res.is_ok(),
        "server exited with error: {server_res:?}"
    );
    assert_eq!(lifecycle.state(), LifecycleState::Terminated);
    assert_eq!(availability.state(), AvailabilityState::Unknown);
    assert!(
        shutdown_hook_ran.load(Ordering::SeqCst),
        "on_shutdown hook must run after TLS drain"
    );
}

#[cfg(feature = "tls")]
#[tokio::test]
async fn rume_builder_tls_negotiates_h2_alpn() {
    use std::sync::Arc;

    const TEST_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----
MIIBwDCCAWWgAwIBAgIUIi3+ZI/tJFK4wTVlTFPD6QkG8XAwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTEyNjEyMTYwMloYDzIxMDAxMDI5
MTIxNjAyWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAATIw5glQvkynffHCd5iEclrqe+juD6xFx8A1F43+PqLjpra4wZ3non1
Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrryo4GSMIGPMB0GA1UdDgQWBBQQ8rRAB+Bs
+SsH4WcR+cJEbH+hTzAfBgNVHSMEGDAWgBQQ8rRAB+Bs+SsH4WcR+cJEbH+hTzAM
BgNVHRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDATBgNVHSUEDDAKBggrBgEFBQcD
ATAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwCgYIKoZIzj0EAwIDSQAwRgIh
APyYGMmbXQV05SjOkoWARBTijDUQ9v3rvdTvfFFJ8HXSAiEA/rJYP8/AcQalmlJL
MOAtrICgSzS3UyLAhl16728zqGc=
-----END CERTIFICATE-----
";

    const TEST_KEY_PEM: &[u8] = b"\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgo2otzH6WgCrTvKb8
Mad8NskKE+yg2Tqk/S0mX41X1AGhRANCAATIw5glQvkynffHCd5iEclrqe+juD6x
Fx8A1F43+PqLjpra4wZ3non1Dp6cLKCLOtYHLB05Rf2+7jxnMmyDVrry
-----END PRIVATE KEY-----
";

    let tls_config = rume_http::tls::TlsConfig::from_pem_bytes(TEST_CERT_PEM, TEST_KEY_PEM)
        .expect("valid PEM cert and key");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let app = Rume::new()
        .tls(tls_config)
        .get("/h2-ping", || async { "ok" });
    let lifecycle = app.lifecycle();

    let mut server = tokio::spawn(async move {
        app.run_with_listener_and_signal(listener, async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;

    let mut root_store = rustls::RootCertStore::empty();
    let mut cert_reader = std::io::BufReader::new(TEST_CERT_PEM);
    for cert in rustls_pemfile::certs(&mut cert_reader) {
        root_store.add(cert.unwrap()).unwrap();
    }
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let mut client_cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    client_cfg.alpn_protocols = vec![b"h2".to_vec()];

    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();
    assert_eq!(
        tls_stream.get_ref().1.alpn_protocol(),
        Some(b"h2".as_slice()),
        "server must negotiate h2 ALPN over TLS"
    );
    drop(tls_stream);

    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn rume_builder_realtime_attaches_hub_and_drains_on_shutdown() {
    let hub = rume_http::realtime::RealtimeHub::new(
        rume_http::realtime::RealtimeConfig::default()
            .with_shutdown_drain_timeout(Duration::from_millis(200)),
    );
    let hub_check = hub.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server = tokio::spawn(async move {
        Rume::new()
            .realtime(hub)
            .get(
                "/hub-node",
                |axum::Extension(h): axum::Extension<rume_http::realtime::RealtimeHub>| async move {
                    h.node_id().to_string()
                },
            )
            .run_with_listener_and_signal(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let mut tcp = None;
    for _ in 0..40 {
        if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
            tcp = Some(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut tcp = tcp.expect("server did not start listening");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tcp.write_all(b"GET /hub-node HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains(hub_check.node_id()));

    assert!(!hub_check.is_shutting_down());
    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
    assert!(hub_check.is_shutting_down());
}

#[tokio::test]
async fn task_mode_runs_selected_task_with_startup_and_shutdown() {
    struct TaskLifecycleModule {
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl crate::module::Module for TaskLifecycleModule {
        fn name(&self) -> &str {
            "task-lifecycle"
        }

        fn on_start<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, crate::module::ModuleHookResult> {
            Box::pin(async move {
                self.events.lock().unwrap().push("on_start".to_string());
                Ok(())
            })
        }

        fn on_shutdown<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, ()> {
            Box::pin(async move {
                self.events.lock().unwrap().push("on_shutdown".to_string());
            })
        }
    }

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let task_events = std::sync::Arc::clone(&events);

    let ctx = AppContext::new(
        (),
        (),
        ProjectPaths::from_current_dir(),
        Availability::new(),
        HealthChecks::new(),
        "task-state-value".to_string(),
    );

    RumeBuilder::new(ctx)
        .module(TaskLifecycleModule {
            events: std::sync::Arc::clone(&events),
        })
        .task(
            "greet",
            "Read state from AppContext",
            move |ctx| async move {
                task_events
                    .lock()
                    .unwrap()
                    .push(format!("task:{}", ctx.state));
                Ok::<_, std::io::Error>(())
            },
        )
        .mode(ApplicationMode::Task)
        .task_name("greet")
        .run_with_signal(std::future::pending())
        .await
        .expect("task mode should execute and shut down cleanly");

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "on_start".to_string(),
            "task:task-state-value".to_string(),
            "on_shutdown".to_string(),
        ]
    );
}

#[tokio::test]
async fn task_mode_unknown_task_returns_not_found_after_shutdown() {
    struct ShutdownTrackerModule {
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl crate::module::Module for ShutdownTrackerModule {
        fn name(&self) -> &str {
            "shutdown-tracker"
        }

        fn on_start<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, crate::module::ModuleHookResult> {
            Box::pin(async move {
                self.events.lock().unwrap().push("on_start".to_string());
                Ok(())
            })
        }

        fn on_shutdown<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, ()> {
            Box::pin(async move {
                self.events.lock().unwrap().push("on_shutdown".to_string());
            })
        }
    }

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ctx = test_ctx();

    let err = RumeBuilder::new(ctx)
        .module(ShutdownTrackerModule {
            events: std::sync::Arc::clone(&events),
        })
        .task("existing", "An existing task", |_ctx| async {
            Ok::<_, std::io::Error>(())
        })
        .mode(ApplicationMode::Task)
        .task_name("missing")
        .run_with_signal(std::future::pending())
        .await
        .expect_err("unknown task should return NotFound");

    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        *events.lock().unwrap(),
        vec!["on_start".to_string(), "on_shutdown".to_string()]
    );
}

#[tokio::test]
async fn openapi_mode_collects_app_and_module_routes_without_binding() {
    struct OpenApiTestModule {
        started: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl crate::module::Module for OpenApiTestModule {
        fn name(&self) -> &str {
            "openapi-test-module"
        }

        fn register_routes(
            &self,
            router: rume_http::routing::Router<()>,
        ) -> rume_http::routing::Router<()> {
            router.get("/module-items", || async { "items" })
        }

        fn on_start<'a>(
            &'a self,
            _ctx: &'a crate::module::ModuleContext,
        ) -> crate::module::BoxFuture<'a, crate::module::ModuleHookResult> {
            Box::pin(async move {
                self.started
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }
    }

    let module_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_flag = std::sync::Arc::clone(&ready_called);

    let ctx = test_ctx();
    let result = RumeBuilder::new(ctx)
        .bind("127.0.0.1:0")
        .module(OpenApiTestModule {
            started: std::sync::Arc::clone(&module_started),
        })
        .get("/app-health", || async { "ok" })
        .openapi_info(rume_http::openapi::OpenApiInfo::new("Test API", "1.2.3"))
        .on_ready(move |_ctx| async move {
            ready_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .mode(ApplicationMode::OpenApi)
        .run_with_signal(std::future::pending())
        .await;

    assert!(result.is_ok(), "OpenApi mode should succeed: {result:?}");
    assert!(
        !module_started.load(std::sync::atomic::Ordering::SeqCst),
        "OpenApi mode must not run module on_start hooks"
    );
    assert!(
        !ready_called.load(std::sync::atomic::Ordering::SeqCst),
        "OpenApi mode must not run application on_ready hooks"
    );
}

#[tokio::test]
async fn deferred_layer_wraps_module_routes_and_later_routes() {
    use axum::body::Body;
    use axum::http::{HeaderValue, Request, StatusCode};
    use tower::ServiceExt;

    struct LayerTestModule;

    impl crate::module::Module for LayerTestModule {
        fn name(&self) -> &str {
            "layer-test-module"
        }

        fn register_routes(
            &self,
            router: rume_http::routing::Router<()>,
        ) -> rume_http::routing::Router<()> {
            router.get("/module-ping", || async { "pong" })
        }
    }

    let ctx = test_ctx();
    let router = RumeBuilder::new(ctx)
        .module(LayerTestModule)
        .layer(axum::middleware::map_response(
            |mut res: axum::response::Response| async move {
                res.headers_mut().insert(
                    axum::http::header::HeaderName::from_static("x-deferred-layer"),
                    HeaderValue::from_static("applied"),
                );
                res
            },
        ))
        .get("/app-ping", || async { "app-pong" })
        .build_router(test_ctx())
        .expect("build_router should succeed");

    let app = router.with_state(test_ctx());

    let mod_res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/module-ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mod_res.status(), StatusCode::OK);
    assert_eq!(
        mod_res
            .headers()
            .get("x-deferred-layer")
            .and_then(|v| v.to_str().ok()),
        Some("applied")
    );

    let app_res = app
        .oneshot(
            Request::builder()
                .uri("/app-ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(app_res.status(), StatusCode::OK);
    assert_eq!(
        app_res
            .headers()
            .get("x-deferred-layer")
            .and_then(|v| v.to_str().ok()),
        Some("applied")
    );
}

#[tokio::test]
async fn plain_http_shutdown_completes_within_shutdown_timeout_with_stalled_connection() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncWriteExt;

    let handler_entered = Arc::new(tokio::sync::Notify::new());
    let handler_entered_clone = Arc::clone(&handler_entered);
    let shutdown_hook_ran = Arc::new(AtomicBool::new(false));
    let shutdown_hook_ran_clone = Arc::clone(&shutdown_hook_ran);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server = tokio::spawn(async move {
        Rume::new()
            .shutdown_timeout(Duration::from_millis(120))
            .get("/stall", move || {
                let entered = Arc::clone(&handler_entered_clone);
                async move {
                    entered.notify_one();
                    std::future::pending::<&'static str>().await
                }
            })
            .on_shutdown(move |_ctx| async move {
                shutdown_hook_ran_clone.store(true, Ordering::SeqCst);
            })
            .run_with_listener_and_signal(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(b"GET /stall HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    handler_entered.notified().await;

    let started = tokio::time::Instant::now();
    let _ = shutdown_tx.send(());

    let res = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("plain-HTTP shutdown must not hang past shutdown_timeout with a stalled client")
        .expect("server task should not panic");
    res.expect("server should complete shutdown cleanly");

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown should complete promptly after shutdown_timeout"
    );
    assert!(
        shutdown_hook_ran.load(Ordering::SeqCst),
        "on_shutdown hook must run after plain-HTTP drain timeout"
    );
    drop(tcp);
}

#[tokio::test]
async fn builder_layer_before_http_cache_isolates_tenant_cached_responses() {
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderName, Request, StatusCode};
    use rume_data::cache::InMemoryCacheBackend;
    use rume_http::http_cache::{HttpCacheConfig, HttpCacheLayer};
    use rume_security::tenant::Tenant;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    let backend = Arc::new(InMemoryCacheBackend::new());
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);

    let inject_tenant_layer = axum::middleware::from_fn(
        |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
            if let Some(tid) = req
                .headers()
                .get(HeaderName::from_static("x-tenant-id"))
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned)
            {
                req.extensions_mut().insert(Tenant::new(tid.clone(), tid));
            }
            next.run(req).await
        },
    );

    let cache = HttpCacheLayer::new(backend, HttpCacheConfig::default());

    let router = RumeBuilder::new(test_ctx())
        .get("/tenant-data", move |req: axum::extract::Request| {
            let hits = Arc::clone(&hits_clone);
            async move {
                let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                let tid = req
                    .extensions()
                    .get::<Tenant>()
                    .map(|t| t.id.clone())
                    .unwrap_or_else(|| "none".to_string());
                format!("tenant={tid},hit={n}")
            }
        })
        .layer(inject_tenant_layer)
        .http_cache(cache)
        .build_router(test_ctx())
        .expect("build_router should succeed");

    let app = router.with_state(test_ctx());

    let r_a1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/tenant-data")
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r_a1.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(r_a1.into_body(), usize::MAX).await.unwrap(),
        b"tenant=tenant-a,hit=1".as_slice()
    );

    // Second tenant must NOT receive tenant-a's cached response.
    let r_b1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/tenant-data")
                .header("x-tenant-id", "tenant-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r_b1.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(r_b1.into_body(), usize::MAX).await.unwrap(),
        b"tenant=tenant-b,hit=2".as_slice()
    );

    // Repeat request for tenant-a hits tenant-a's cache entry.
    let r_a2 = app
        .oneshot(
            Request::builder()
                .uri("/tenant-data")
                .header("x-tenant-id", "tenant-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r_a2.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(r_a2.into_body(), usize::MAX).await.unwrap(),
        b"tenant=tenant-a,hit=1".as_slice()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
