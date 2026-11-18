//! Extension trait providing `.test_app()` and `.build_test_app()` on [`RumeBuilder`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rume_app::app::builder::{BuildError, RumeBuilder};
use rume_app::app::state::{ApplicationMode, AvailabilityState, LifecycleState};
use rume_app::module::{Module, ModuleContext, ModuleRegistry};

use crate::app::TestApp;

/// Extension trait adding test application builders to [`RumeBuilder`].
///
/// Implemented in `rume-testing` so `rume-app` does not depend on `rume-testing`.
pub trait RumeTestingExt {
    /// Build the application router and return a lightweight [`TestApp`]
    /// for integration and unit testing without binding a network port.
    fn test_app(self) -> Result<TestApp, std::io::Error>;

    /// Asynchronously builds the application and returns a [`TestApp`]
    /// after executing all module `on_start` lifecycle hooks and `on_ready` hooks in topological order.
    ///
    /// Automatically attaches an explicit teardown hook to [`TestApp`] that executes
    /// module `on_shutdown` hooks in reverse topological order upon teardown or drop.
    fn build_test_app(self) -> Pin<Box<dyn Future<Output = Result<TestApp, BuildError>> + Send>>;
}

impl<E, C, S> RumeTestingExt for RumeBuilder<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    fn test_app(self) -> Result<TestApp, std::io::Error> {
        let parts = self.into_test_parts().map_err(std::io::Error::from)?;
        parts.ctx.lifecycle.set(LifecycleState::Ready);
        parts.ctx.availability.set(AvailabilityState::Ready);
        Ok(TestApp::new(parts.router))
    }

    fn build_test_app(self) -> Pin<Box<dyn Future<Output = Result<TestApp, BuildError>> + Send>> {
        Box::pin(async move {
            let parts = self.into_test_parts()?;
            let mut module_registry = ModuleRegistry::new();
            for m in &parts.modules {
                module_registry.register(Arc::clone(m))?;
            }
            let startup_modules = module_registry.resolve_startup_modules()?;
            let shutdown_modules = module_registry.resolve_shutdown_modules()?;

            let ctx = parts.ctx;
            let mut on_shutdown = parts.on_shutdown;
            let on_ready = parts.on_ready;
            let shutdown_timeout = parts.shutdown_timeout;

            // In Inspect mode, align with run(): boot context to Booted/Live and return without running hooks.
            if parts.mode == ApplicationMode::Inspect {
                ctx.lifecycle.set(LifecycleState::Booted);
                ctx.availability.set(AvailabilityState::Live);
                return Ok(TestApp::new(parts.router));
            }

            // 1. Booted & Live
            ctx.lifecycle.set(LifecycleState::Booted);
            ctx.availability.set(AvailabilityState::Live);

            // 2. Run module on_start hooks in topological order with rollback
            let mut started_modules: Vec<Arc<dyn Module>> = Vec::new();
            for m in startup_modules {
                let mod_ctx = ModuleContext::new(
                    m.name(),
                    ctx.paths.clone(),
                    ctx.availability.clone(),
                    ctx.lifecycle.clone(),
                );
                if let Err(err) = m.on_start(&mod_ctx).await {
                    ctx.lifecycle.set(LifecycleState::Terminating);
                    ctx.availability.set(AvailabilityState::Broken);

                    started_modules.reverse();
                    let cleanup_ctx = ctx.clone();
                    for hook in std::mem::take(&mut on_shutdown) {
                        let _ = hook(cleanup_ctx.clone()).await;
                    }
                    for sm in started_modules {
                        let sm_ctx = ModuleContext::new(
                            sm.name(),
                            cleanup_ctx.paths.clone(),
                            cleanup_ctx.availability.clone(),
                            cleanup_ctx.lifecycle.clone(),
                        );
                        sm.on_shutdown(&sm_ctx).await;
                    }
                    ctx.lifecycle.set(LifecycleState::Terminated);
                    return Err(BuildError::Startup(format!(
                        "module '{}' failed to start: {err}",
                        m.name()
                    )));
                }
                started_modules.push(m);
            }

            // 3. Application on_ready hooks
            for hook in on_ready {
                if let Err(err) = hook(ctx.clone()).await {
                    ctx.lifecycle.set(LifecycleState::Terminating);
                    ctx.availability.set(AvailabilityState::Broken);

                    started_modules.reverse();
                    let cleanup_ctx = ctx.clone();
                    for hook in std::mem::take(&mut on_shutdown) {
                        let _ = hook(cleanup_ctx.clone()).await;
                    }
                    for sm in started_modules {
                        let sm_ctx = ModuleContext::new(
                            sm.name(),
                            cleanup_ctx.paths.clone(),
                            cleanup_ctx.availability.clone(),
                            cleanup_ctx.lifecycle.clone(),
                        );
                        sm.on_shutdown(&sm_ctx).await;
                    }
                    ctx.lifecycle.set(LifecycleState::Terminated);
                    return Err(BuildError::Startup(format!("startup hook failed: {err}")));
                }
            }

            ctx.lifecycle.set(LifecycleState::Ready);
            ctx.availability.set(AvailabilityState::Ready);

            // 4. Attach teardown closure
            let cleanup_ctx = ctx.clone();
            let cleanup = Box::new(move || {
                let shutdown_future = async move {
                    cleanup_ctx.lifecycle.set(LifecycleState::Terminating);
                    cleanup_ctx
                        .availability
                        .set(AvailabilityState::RefusingTraffic);

                    for hook in on_shutdown {
                        let _ = hook(cleanup_ctx.clone()).await;
                    }
                    for m in shutdown_modules {
                        let sm_ctx = ModuleContext::new(
                            m.name(),
                            cleanup_ctx.paths.clone(),
                            cleanup_ctx.availability.clone(),
                            cleanup_ctx.lifecycle.clone(),
                        );
                        m.on_shutdown(&sm_ctx).await;
                    }
                    cleanup_ctx.lifecycle.set(LifecycleState::Terminated);
                };

                let _ = std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(async {
                            let _ = tokio::time::timeout(shutdown_timeout, shutdown_future).await;
                        });
                    }
                })
                .join();
            });

            Ok(TestApp::with_cleanup(parts.router, cleanup))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rume_app::Rume;
    use rume_app::module::{BoxFuture, Module, ModuleContext, ModuleHookResult};

    use super::RumeTestingExt;

    struct LifecycleModule {
        name: &'static str,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl Module for LifecycleModule {
        fn name(&self) -> &str {
            self.name
        }

        fn on_start<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ModuleHookResult> {
            Box::pin(async move {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("start:{}", self.name));
                Ok(())
            })
        }

        fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("shutdown:{}", self.name));
            })
        }
    }

    #[tokio::test]
    async fn testing_ext_builds_test_app_and_runs_teardown_hooks() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let app = Rume::new()
            .module(LifecycleModule {
                name: "alpha",
                events: Arc::clone(&events),
            })
            .build_test_app()
            .await
            .expect("build_test_app should succeed");

        assert_eq!(*events.lock().unwrap(), vec!["start:alpha"]);
        drop(app);
        assert_eq!(
            *events.lock().unwrap(),
            vec!["start:alpha", "shutdown:alpha"]
        );
    }
}
