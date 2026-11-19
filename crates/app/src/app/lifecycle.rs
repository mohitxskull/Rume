//! Shared application lifecycle coordinator for startup, rollback, and shutdown.
//!
//! Owned by `rume-app` and shared between [`crate::app::builder::RumeBuilder`]
//! (live server runtime) and `rume-testing`'s `RumeTestingExt::build_test_app`
//! so both paths execute identical module DAG resolution, hook ordering,
//! failure rollback, and bounded-timeout shutdown transitions.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::timeout;

use crate::app::builder::{BuildError, ReadyHook, ShutdownHook};
use crate::app::context::AppContext;
use crate::app::state::{AvailabilityState, LifecycleState};
use crate::module::{Module, ModuleContext, ModuleError, ModuleRegistry};

/// Modules resolved into topological startup and reverse shutdown order.
///
/// Shared by [`crate::app::builder::RumeBuilder`] and `rume-testing`'s
/// `RumeTestingExt` so module ordering is validated once and reused consistently.
#[derive(Clone)]
pub struct ResolvedModules {
    /// Modules in dependency-first startup order.
    pub startup: Vec<Arc<dyn Module>>,
    /// Modules in dependent-first reverse shutdown order.
    pub shutdown: Vec<Arc<dyn Module>>,
}

/// Errors produced while resolving or running application lifecycle hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// Module registration or dependency graph resolution failed.
    Module(ModuleError),
    /// A module `on_start` hook or application `on_ready` hook returned an error.
    Startup(String),
    /// A shutdown signal interrupted startup before the application reached Ready.
    Interrupted(String),
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Module(err) => write!(f, "{err}"),
            Self::Startup(msg) | Self::Interrupted(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for LifecycleError {}

impl From<ModuleError> for LifecycleError {
    fn from(err: ModuleError) -> Self {
        Self::Module(err)
    }
}

impl From<LifecycleError> for BuildError {
    fn from(err: LifecycleError) -> Self {
        match err {
            LifecycleError::Module(err) => Self::Module(err),
            LifecycleError::Startup(msg) | LifecycleError::Interrupted(msg) => Self::Startup(msg),
        }
    }
}

impl From<LifecycleError> for std::io::Error {
    fn from(err: LifecycleError) -> Self {
        match err {
            LifecycleError::Module(err) => err.into(),
            LifecycleError::Startup(msg) => std::io::Error::other(msg),
            LifecycleError::Interrupted(msg) => {
                std::io::Error::new(std::io::ErrorKind::Interrupted, msg)
            }
        }
    }
}

/// Register all modules in a [`ModuleRegistry`] and resolve startup/shutdown order.
///
/// Shared by [`crate::app::builder::RumeBuilder`] and `rume-testing`'s `RumeTestingExt`.
pub fn resolve_modules(modules: &[Arc<dyn Module>]) -> Result<ResolvedModules, ModuleError> {
    let mut registry = ModuleRegistry::new();
    for module in modules {
        registry.register(Arc::clone(module))?;
    }
    let startup = registry.resolve_startup_modules()?;
    let shutdown = registry.resolve_shutdown_modules()?;
    Ok(ResolvedModules { startup, shutdown })
}

/// Execute module `on_start` hooks in topological order followed by `on_ready` hooks,
/// rolling back already-started modules and `on_shutdown` hooks on failure or signal.
///
/// Shared by [`crate::app::builder::RumeBuilder`] (passing `Some(shutdown_rx)`)
/// and `rume-testing`'s `RumeTestingExt::build_test_app` (passing `None`).
pub async fn run_startup<E, C, S>(
    ctx: &AppContext<E, C, S>,
    startup_modules: &[Arc<dyn Module>],
    on_ready: Vec<ReadyHook<E, C, S>>,
    on_shutdown: &mut Vec<ShutdownHook<E, C, S>>,
    shutdown_timeout: Duration,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> Result<(), LifecycleError>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    ctx.lifecycle.set(LifecycleState::Booted);
    ctx.availability.set(AvailabilityState::Live);

    let mut started_modules: Vec<Arc<dyn Module>> = Vec::with_capacity(startup_modules.len());

    for module in startup_modules {
        let mod_ctx = ModuleContext::new(
            module.name(),
            ctx.paths.clone(),
            ctx.availability.clone(),
            ctx.lifecycle.clone(),
        );

        // Why: Poll the latched shutdown receiver first (`biased;`) when present so an
        // already-received signal aborts boot deterministically rather than racing an
        // immediately-ready hook; passing `None` lets `build_test_app` reuse the exact
        // same startup and rollback state machine via `pending()`.
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown_rx.as_ref()) => {
                tracing::warn!("shutdown signal received during module startup; aborting boot");
                rollback_on_signal(ctx, on_shutdown, started_modules, shutdown_timeout).await;
                return Err(LifecycleError::Interrupted(
                    "shutdown signal received during module startup".to_string(),
                ));
            }
            res = module.on_start(&mod_ctx) => {
                if let Err(err) = res {
                    tracing::error!(
                        "module '{}' failed to start: {err}; executing cleanup",
                        module.name()
                    );
                    rollback_on_failure(ctx, on_shutdown, started_modules, shutdown_timeout).await;
                    return Err(LifecycleError::Startup(format!(
                        "module '{}' failed to start: {err}",
                        module.name()
                    )));
                }
            }
        }
        started_modules.push(Arc::clone(module));
    }

    for hook in on_ready {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown_rx.as_ref()) => {
                tracing::warn!(
                    "shutdown signal received during application startup hooks; aborting boot"
                );
                rollback_on_signal(ctx, on_shutdown, started_modules, shutdown_timeout).await;
                return Err(LifecycleError::Interrupted(
                    "shutdown signal received during application startup".to_string(),
                ));
            }
            res = hook(ctx.clone()) => {
                if let Err(err) = res {
                    tracing::error!(
                        "startup hook failed: {err}; executing registered cleanup hooks"
                    );
                    rollback_on_failure(ctx, on_shutdown, started_modules, shutdown_timeout).await;
                    return Err(LifecycleError::Startup(format!(
                        "startup hook failed: {err}"
                    )));
                }
            }
        }
    }

    ctx.lifecycle.set(LifecycleState::Ready);
    ctx.availability.set(AvailabilityState::Ready);
    Ok(())
}

/// Run application `on_shutdown` hooks followed by module `on_shutdown` hooks in
/// reverse topological order under the configured aggregate `shutdown_timeout`.
///
/// Shared by [`crate::app::builder::RumeBuilder`] (post-drain shutdown) and
/// `rume-testing`'s `RumeTestingExt::build_test_app` (teardown/drop closure).
pub async fn run_shutdown<E, C, S>(
    ctx: &AppContext<E, C, S>,
    on_shutdown: Vec<ShutdownHook<E, C, S>>,
    shutdown_modules: Vec<Arc<dyn Module>>,
    shutdown_timeout: Duration,
) where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    execute_cleanup(
        ctx,
        on_shutdown,
        shutdown_modules,
        shutdown_timeout,
        AvailabilityState::RefusingTraffic,
        Some(AvailabilityState::Unknown),
    )
    .await;
}

async fn rollback_on_failure<E, C, S>(
    ctx: &AppContext<E, C, S>,
    on_shutdown: &mut Vec<ShutdownHook<E, C, S>>,
    mut started_modules: Vec<Arc<dyn Module>>,
    shutdown_timeout: Duration,
) where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    started_modules.reverse();
    // Why: Leave availability at `Broken` (rather than resetting to `Unknown`) when
    // startup fails so probes/observers retain failure diagnosis after cleanup finishes.
    execute_cleanup(
        ctx,
        std::mem::take(on_shutdown),
        started_modules,
        shutdown_timeout,
        AvailabilityState::Broken,
        None,
    )
    .await;
}

async fn rollback_on_signal<E, C, S>(
    ctx: &AppContext<E, C, S>,
    on_shutdown: &mut Vec<ShutdownHook<E, C, S>>,
    mut started_modules: Vec<Arc<dyn Module>>,
    shutdown_timeout: Duration,
) where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    started_modules.reverse();
    execute_cleanup(
        ctx,
        std::mem::take(on_shutdown),
        started_modules,
        shutdown_timeout,
        AvailabilityState::RefusingTraffic,
        Some(AvailabilityState::Unknown),
    )
    .await;
}

async fn execute_cleanup<E, C, S>(
    ctx: &AppContext<E, C, S>,
    on_shutdown: Vec<ShutdownHook<E, C, S>>,
    modules: Vec<Arc<dyn Module>>,
    shutdown_timeout: Duration,
    start_availability: AvailabilityState,
    end_availability: Option<AvailabilityState>,
) where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    ctx.lifecycle.set(LifecycleState::Terminating);
    ctx.availability.set(start_availability);

    let cleanup_ctx = ctx.clone();
    let cleanup_fut = async move {
        for hook in on_shutdown {
            hook(cleanup_ctx.clone()).await;
        }
        for module in modules {
            let mod_ctx = ModuleContext::new(
                module.name(),
                cleanup_ctx.paths.clone(),
                cleanup_ctx.availability.clone(),
                cleanup_ctx.lifecycle.clone(),
            );
            module.on_shutdown(&mod_ctx).await;
        }
    };

    if timeout(shutdown_timeout, cleanup_fut).await.is_err() {
        tracing::warn!(
            "shutdown hooks exceeded {}s timeout in aggregate, aborting remaining hooks",
            shutdown_timeout.as_secs()
        );
    }

    ctx.lifecycle.set(LifecycleState::Terminated);
    if let Some(availability) = end_availability {
        ctx.availability.set(availability);
    }
}

async fn wait_for_shutdown(rx: Option<&watch::Receiver<bool>>) {
    match rx {
        Some(rx) => {
            let mut rx = rx.clone();
            let _ = rx.wait_for(|&shutting_down| shutting_down).await;
        }
        None => std::future::pending::<()>().await,
    }
}
