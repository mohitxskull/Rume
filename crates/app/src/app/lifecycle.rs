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
    let mut shutdown = startup.clone();
    shutdown.reverse();
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
        // immediately-ready hook; wrap `on_start` in an `async` block so `select!` does
        // not eagerly invoke the hook before polling the signal branch. Passing `None`
        // lets `build_test_app` reuse the exact same startup and rollback state machine
        // via `pending()`.
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown_rx.as_ref()) => {
                tracing::warn!("shutdown signal received during module startup; aborting boot");
                rollback_on_signal(ctx, on_shutdown, started_modules, shutdown_timeout).await;
                return Err(LifecycleError::Interrupted(
                    "shutdown signal received during module startup".to_string(),
                ));
            }
            res = async { module.on_start(&mod_ctx).await } => {
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
            res = async { hook(ctx.clone()).await } => {
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

    // Why: when both `startup_modules` and `on_ready` are empty (or shutdown latched right as
    // the last hook completed), neither `select!` catches it; check the latched receiver
    // before publishing `Ready` so we never transition to `Ready` after shutdown was signaled.
    if shutdown_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        tracing::warn!("shutdown signal received before Ready transition; aborting boot");
        rollback_on_signal(ctx, on_shutdown, started_modules, shutdown_timeout).await;
        return Err(LifecycleError::Interrupted(
            "shutdown signal received before application reached Ready".to_string(),
        ));
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

/// Waits until the shutdown channel latches `true`.
///
/// If all senders are dropped while the channel still holds `false`, signal monitoring
/// is treated as disabled (parks permanently) rather than falsely aborting startup.
/// A channel whose final latched value is `true` still interrupts startup immediately.
async fn wait_for_shutdown(rx: Option<&watch::Receiver<bool>>) {
    match rx {
        Some(rx) => {
            let mut rx = rx.clone();
            if rx.wait_for(|&shutting_down| shutting_down).await.is_err() {
                if *rx.borrow() {
                    return;
                }
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::app::paths::ProjectPaths;
    use crate::app::state::Availability;
    use crate::module::BoxFuture;
    use rume_foundation::health::HealthChecks;

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

    #[tokio::test]
    async fn pre_latched_shutdown_prevents_eager_on_start_and_ready_transition() {
        struct EagerSideEffectModule {
            invoked: Arc<AtomicBool>,
        }
        impl Module for EagerSideEffectModule {
            fn name(&self) -> &'static str {
                "eager"
            }
            fn on_start<'a>(
                &'a self,
                _ctx: &'a ModuleContext,
            ) -> BoxFuture<'a, Result<(), Box<dyn std::error::Error + Send + Sync>>> {
                self.invoked.store(true, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        }

        // Case 1: pre-latched shutdown must NOT invoke `on_start` even synchronously.
        let invoked = Arc::new(AtomicBool::new(false));
        let module: Arc<dyn Module> = Arc::new(EagerSideEffectModule {
            invoked: Arc::clone(&invoked),
        });
        let (_tx, rx) = watch::channel(true);
        let ctx = test_ctx();
        let mut on_shutdown = Vec::new();
        let err = run_startup(
            &ctx,
            &[module],
            Vec::new(),
            &mut on_shutdown,
            Duration::from_secs(1),
            Some(rx),
        )
        .await
        .expect_err("pre-latched shutdown must interrupt startup");
        assert!(matches!(err, LifecycleError::Interrupted(_)));
        assert!(
            !invoked.load(Ordering::SeqCst),
            "on_start must not be invoked when shutdown is already latched"
        );

        // Case 2: empty startup_modules and on_ready with pre-latched shutdown must NOT publish Ready.
        let (_tx2, rx2) = watch::channel(true);
        let ctx2 = test_ctx();
        let mut on_shutdown2 = Vec::new();
        let err2 = run_startup(
            &ctx2,
            &[],
            Vec::new(),
            &mut on_shutdown2,
            Duration::from_secs(1),
            Some(rx2),
        )
        .await
        .expect_err("pre-latched shutdown with empty hooks must interrupt before Ready");
        assert!(matches!(err2, LifecycleError::Interrupted(_)));
        assert_ne!(ctx2.lifecycle.state(), LifecycleState::Ready);
    }

    #[tokio::test]
    async fn dropped_shutdown_sender_with_false_does_not_abort_startup() {
        let (tx, rx) = watch::channel(false);
        drop(tx);

        let ctx = test_ctx();
        let mut on_shutdown = Vec::new();
        let ready_ran = Arc::new(AtomicBool::new(false));
        let spy = Arc::clone(&ready_ran);
        let hook: ReadyHook<(), (), ()> = Box::new(move |_| {
            let spy = Arc::clone(&spy);
            Box::pin(async move {
                tokio::task::yield_now().await;
                spy.store(true, Ordering::SeqCst);
                Ok(())
            })
        });

        run_startup(
            &ctx,
            &[],
            vec![hook],
            &mut on_shutdown,
            Duration::from_secs(1),
            Some(rx),
        )
        .await
        .expect("closed shutdown channel holding false must not abort startup");
        assert!(ready_ran.load(Ordering::SeqCst));
        assert_eq!(ctx.lifecycle.state(), LifecycleState::Ready);
    }
}
