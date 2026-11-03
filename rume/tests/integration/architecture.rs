use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use rume::module::{
    ArchitectureRules, BoxFuture, Module, ModuleContext, ModuleHarness, ModuleHookResult,
};
use rume::prelude::*;

struct UsersModule {
    started: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl Module for UsersModule {
    fn name(&self) -> &'static str {
        "users"
    }

    fn on_start<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ModuleHookResult> {
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            started.store(true, Ordering::SeqCst);
            Ok(())
        })
    }

    fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ()> {
        let shutdown = Arc::clone(&self.shutdown);
        Box::pin(async move {
            shutdown.store(true, Ordering::SeqCst);
        })
    }

    fn register_routes(&self, router: Router) -> Router {
        router.get("/users/me", || async { "current_user" })
    }
}

#[tokio::test]
async fn module_harness_isolated_lifecycle_and_testing() {
    let started = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));

    let module = UsersModule {
        started: Arc::clone(&started),
        shutdown: Arc::clone(&shutdown),
    };

    let harness = ModuleHarness::for_module(module);
    assert_eq!(harness.context().module_name(), "users");
    assert!(!started.load(Ordering::SeqCst));

    // 1. Run start hook
    harness.start().await.expect("start should succeed");
    assert!(started.load(Ordering::SeqCst));

    // 2. Test isolated route
    let app = harness.test_app();
    let res = app.get("/users/me").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "current_user");

    // 3. Run shutdown hook
    harness.shutdown().await;
    assert!(shutdown.load(Ordering::SeqCst));
    // Repeated shutdown is safe and idempotent
    harness.shutdown().await;
}

#[derive(Debug)]
struct FailingDbCheck;

impl rume::routing::HealthCheck for FailingDbCheck {
    fn name(&self) -> &str {
        "failing_db"
    }

    fn run(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = rume::routing::HealthCheckResult> + Send>>
    {
        Box::pin(async {
            rume::routing::HealthCheckResult {
                name: "failing_db".to_string(),
                status: rume::routing::HealthStatus::Failed,
                message: "database unavailable".to_string(),
                meta: std::collections::BTreeMap::new(),
                cached: false,
            }
        })
    }
}

struct FailingModule;

impl Module for FailingModule {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn on_start<'a>(
        &'a self,
        _ctx: &'a ModuleContext,
    ) -> rume::module::BoxFuture<'a, rume::module::ModuleHookResult> {
        Box::pin(async { Err("db connection refused".into()) })
    }

    fn register_health_checks(&self, checks: &mut rume::routing::HealthChecks) {
        checks.add(FailingDbCheck);
    }
}

#[tokio::test]
async fn module_harness_health_checks_and_error_paths() {
    let harness = ModuleHarness::for_module(FailingModule);

    // Health check inspection before start
    let report = harness.run_health_checks().await;
    assert_eq!(report.status, "error");
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, "failing_db");
    assert_eq!(report.checks[0].status, rume::routing::HealthStatus::Failed);

    // Startup failure returns ModuleError::StartupFailed
    let err = harness.start().await.unwrap_err();
    match err {
        rume::module::ModuleError::StartupFailed { module, message } => {
            assert_eq!(module, "failing");
            assert!(message.contains("db connection refused"));
        }
        other => panic!("expected StartupFailed, got {other:?}"),
    }

    // Shutdown when start failed or never called is a no-op
    harness.shutdown().await;
}

struct BillingModule {
    deps: Vec<&'static str>,
}

impl Module for BillingModule {
    fn name(&self) -> &'static str {
        "billing"
    }

    fn dependencies(&self) -> Vec<&'static str> {
        self.deps.clone()
    }
}

struct FrontendModule;

impl Module for FrontendModule {
    fn name(&self) -> &'static str {
        "frontend"
    }
}

#[test]
fn architecture_rules_enforce_forbidden_cross_domain_calls() {
    let users = UsersModule {
        started: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(AtomicBool::new(false)),
    };
    let clean_billing = BillingModule {
        deps: vec!["users"],
    };
    let frontend = FrontendModule;

    let rules = ArchitectureRules::new()
        .require_module("users")
        .require_module("billing")
        .forbid_dependency("billing", "frontend");

    // Clean architecture passes
    let modules: Vec<&dyn Module> = vec![&users, &clean_billing, &frontend];
    assert!(rules.verify(&modules).is_ok());

    // Violating architecture fails
    let dirty_billing = BillingModule {
        deps: vec!["users", "frontend"],
    };
    let dirty_modules: Vec<&dyn Module> = vec![&users, &dirty_billing, &frontend];
    let err = rules.verify(&dirty_modules).unwrap_err();

    assert_eq!(err.rule, "forbid_dependency");
    assert!(
        err.details
            .contains("module 'billing' is forbidden from depending on 'frontend'")
    );

    // Missing required module fails
    let missing_rules = ArchitectureRules::new().require_module("inventory");
    let missing_err = missing_rules.verify(&modules).unwrap_err();
    assert_eq!(missing_err.rule, "require_module");
    assert!(
        missing_err
            .details
            .contains("required module 'inventory' is missing")
    );
}

struct PaymentsModule {
    deps: Vec<&'static str>,
}

impl Module for PaymentsModule {
    fn name(&self) -> &'static str {
        "payments"
    }

    fn dependencies(&self) -> Vec<&'static str> {
        self.deps.clone()
    }
}

#[test]
fn architecture_rules_enforce_transitive_dependencies() {
    // A (billing) -> B (payments) -> C (frontend)
    let frontend = FrontendModule;
    let payments = PaymentsModule {
        deps: vec!["frontend"],
    };
    let billing = BillingModule {
        deps: vec!["payments"],
    };

    let rules = ArchitectureRules::new().forbid_dependency("billing", "frontend");

    let modules: Vec<&dyn Module> = vec![&billing, &payments, &frontend];
    let err = rules.verify(&modules).unwrap_err();
    assert_eq!(err.rule, "forbid_dependency");
    assert!(err.details.contains(
        "module 'billing' is forbidden from depending on 'frontend', but reaches it as a dependency"
    ));
}

struct CollisionModuleA;
impl Module for CollisionModuleA {
    fn name(&self) -> &'static str {
        "mod_a"
    }
    fn register_routes(&self, router: Router) -> Router {
        router.get("/conflict", || async { "A" })
    }
}

struct CollisionModuleB;
impl Module for CollisionModuleB {
    fn name(&self) -> &'static str {
        "mod_b"
    }
    fn register_routes(&self, router: Router) -> Router {
        router.get("/conflict", || async { "B" })
    }
}

#[tokio::test]
async fn route_merge_collision_fails_preflight_without_panic() {
    let builder = Rume::new()
        .module(CollisionModuleA)
        .module(CollisionModuleB);

    let res = builder.build_test_app().await;
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(matches!(
        err,
        rume::app::builder::BuildError::DuplicateRoute { ref path, .. } if path == "/conflict"
    ));
}

#[tokio::test]
async fn test_app_from_builder_executes_module_lifecycle() {
    let started = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));

    let module = UsersModule {
        started: Arc::clone(&started),
        shutdown: Arc::clone(&shutdown),
    };

    let builder = Rume::new().module(module);
    let app = TestApp::from_builder(builder)
        .await
        .expect("test app build");

    // on_start hook has already run during build_test_app!
    assert!(started.load(Ordering::SeqCst));
    assert!(!shutdown.load(Ordering::SeqCst));

    // Routes work through test app
    let res = app.get("/users/me").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(TestApp::body_text(res).await, "current_user");

    // Teardown executes on_shutdown hook cleanly
    app.teardown();
    assert!(shutdown.load(Ordering::SeqCst));
}
