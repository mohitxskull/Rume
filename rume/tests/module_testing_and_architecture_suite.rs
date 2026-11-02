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
