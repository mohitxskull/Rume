//! Isolated module integration testing harness.
//!
//! Allows testing an individual application module in complete isolation,
//! executing its lifecycle hooks (`on_start`/`on_shutdown`) and verifying its
//! routes and state without booting the full application.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rume_app::app::{Availability, Lifecycle, ProjectPaths};
use rume_app::module::Module;
use rume_app::module::context::ModuleContext;
use rume_app::module::error::ModuleError;
use rume_http::routing::Router;

use crate::app::TestApp;

/// Convenience alias for [`ModuleHarness`].
pub type ModuleTestHarness<M> = ModuleHarness<M>;

/// An isolated test harness for verifying a single application module.
pub struct ModuleHarness<M: Module> {
    module: Arc<M>,
    ctx: ModuleContext,
    is_started: Arc<AtomicBool>,
}

impl<M: Module> ModuleHarness<M> {
    /// Creates a new test harness for the specified module.
    pub fn for_module(module: M) -> Self {
        let name = module.name();
        let paths = ProjectPaths::from_current_dir();
        let availability = Availability::new();
        let lifecycle = Lifecycle::new();
        let ctx = ModuleContext::new(name, paths, availability, lifecycle);

        Self {
            module: Arc::new(module),
            ctx,
            is_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Accesses the module's scoped context.
    pub fn context(&self) -> &ModuleContext {
        &self.ctx
    }

    /// Accesses the underlying module instance.
    pub fn module(&self) -> &M {
        &self.module
    }

    /// Executes the module's `on_start` lifecycle hook.
    pub async fn start(&self) -> Result<(), ModuleError> {
        let res = self.module.on_start(&self.ctx).await;
        if let Err(err) = res {
            return Err(ModuleError::StartupFailed {
                module: self.module.name().to_string(),
                message: err.to_string(),
            });
        }
        self.is_started.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Executes the module's `on_shutdown` lifecycle hook.
    pub async fn shutdown(&self) {
        if self.is_started.swap(false, Ordering::SeqCst) {
            self.module.on_shutdown(&self.ctx).await;
        }
    }

    /// Builds a lightweight [`TestApp`] containing only the routes owned by this module.
    pub fn test_app(&self) -> TestApp {
        let router = self.module.register_routes(Router::new());
        TestApp::new(router)
    }

    /// Collects all health checks registered by this module.
    pub fn health_checks(&self) -> rume_http::routing::HealthChecks {
        let mut checks = rume_http::routing::HealthChecks::new();
        self.module.register_health_checks(&mut checks);
        checks
    }

    /// Runs all health checks registered by this module and returns their report.
    pub async fn run_health_checks(&self) -> rume_http::routing::HealthReport {
        let checks = self.health_checks();
        checks.run().await
    }
}
