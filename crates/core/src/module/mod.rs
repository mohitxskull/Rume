//! First-class application modules.
//!
//! An application module represents a coherent, independently owned domain
//! boundary (e.g. `UsersModule`, `BillingModule`, `OrdersModule`).
//!
//! A module can encapsulate:
//! - Owned HTTP routes and resources
//! - Domain-specific state and services
//! - Module-specific middleware layers
//! - Diagnostic health checks
//! - Asynchronous startup and shutdown lifecycle hooks
//! - Explicit dependencies on other modules (with cycle detection and topological ordering)

use std::future::Future;
use std::pin::Pin;

use crate::routing::{HealthChecks, Router};

pub mod architecture;
pub mod context;
pub mod error;
pub mod harness;
pub mod registry;

pub use architecture::{ArchitectureRules, ArchitectureViolation};
pub use context::ModuleContext;
pub use error::ModuleError;
pub use harness::ModuleHarness;
pub use registry::ModuleRegistry;

/// Result type returned by fallible module startup hooks.
pub type ModuleHookResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Boxed pinned future for asynchronous module lifecycle hooks.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A first-class application module.
///
/// Implemented by domain components to declare their routes, health checks,
/// lifecycle hooks, and dependencies on other modules.
pub trait Module: Send + Sync + 'static {
    /// The unique name of this module (e.g. `"users"`, `"billing"`).
    fn name(&self) -> &str;

    /// The list of module names that this module explicitly depends on.
    ///
    /// The framework guarantees:
    /// - All listed dependencies must be registered in the application.
    /// - Circular dependencies are detected and rejected during preflight.
    /// - Dependencies initialize **before** this module during startup.
    /// - Dependencies shut down **after** this module during graceful shutdown.
    fn dependencies(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Mount routes owned by this module onto the application router.
    fn register_routes(&self, router: Router) -> Router {
        router
    }

    /// Register operational health checks owned by this module.
    fn register_health_checks(&self, checks: &mut HealthChecks) {
        let _ = checks;
    }

    /// Asynchronous initialization hook for this module.
    ///
    /// Executed during application startup in topological dependency order
    /// (dependencies initialize before dependents). If this hook fails, startup
    /// aborts and any already-started modules are cleanly shut down in reverse order.
    fn on_start<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ModuleHookResult> {
        Box::pin(async { Ok(()) })
    }

    /// Asynchronous shutdown hook for this module.
    ///
    /// Executed during graceful shutdown in reverse topological dependency order
    /// (dependents shut down before the dependencies they rely on).
    fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}
