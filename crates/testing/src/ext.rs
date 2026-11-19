//! Extension trait providing `.test_app()` and `.build_test_app()` on [`RumeBuilder`].

use std::future::Future;
use std::pin::Pin;

use rume_app::app::builder::{BuildError, RumeBuilder};
use rume_app::app::lifecycle;
use rume_app::app::state::{ApplicationMode, AvailabilityState, LifecycleState};

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
            let resolved = lifecycle::resolve_modules(&parts.modules)?;

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

            lifecycle::run_startup(
                &ctx,
                &resolved.startup,
                on_ready,
                &mut on_shutdown,
                shutdown_timeout,
                None,
            )
            .await?;

            // Attach teardown closure that runs synchronous-drop cleanup on a dedicated
            // current-thread runtime so `Drop for TestApp` works even inside an active
            // Tokio runtime without panicking.
            let cleanup_ctx = ctx.clone();
            let shutdown_modules = resolved.shutdown;
            let cleanup = Box::new(move || {
                let _ = std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        rt.block_on(lifecycle::run_shutdown(
                            &cleanup_ctx,
                            on_shutdown,
                            shutdown_modules,
                            shutdown_timeout,
                        ));
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

        fn register_routes(
            &self,
            router: rume_http::routing::Router,
        ) -> rume_http::routing::Router {
            router.get(&format!("/mod-{}", self.name), || async { "ok" })
        }
    }

    #[tokio::test]
    async fn testing_ext_builds_test_app_and_runs_teardown_hooks() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let app = Rume::new()
            .module(LifecycleModule {
                name: "alpha",
                deps: vec![],
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

    #[tokio::test]
    async fn build_test_app_shares_rollback_on_on_ready_failure() {
        use rume_app::app::builder::BuildError;
        use rume_app::app::context::AppContext;
        use rume_app::app::state::{AvailabilityState, LifecycleState};

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = AppContext::new_with_defaults((), ());
        let lifecycle = ctx.lifecycle.clone();
        let availability = ctx.availability.clone();

        let err = Rume::builder(ctx)
            .module(LifecycleModule {
                name: "beta",
                deps: vec!["alpha"],
                events: Arc::clone(&events),
            })
            .module(LifecycleModule {
                name: "alpha",
                deps: vec![],
                events: Arc::clone(&events),
            })
            .on_ready(|ctx| async move {
                assert!(
                    ctx.route_registry
                        .routes()
                        .iter()
                        .any(|r| r.path == "/mod-alpha"),
                    "on_ready in build_test_app must observe module routes"
                );
                Err::<(), _>(std::io::Error::other("ready hook boom"))
            })
            .build_test_app()
            .await
            .expect_err("build_test_app must fail when on_ready fails");

        assert!(
            matches!(err, BuildError::Startup(ref msg) if msg.contains("ready hook boom")),
            "unexpected error: {err:?}"
        );
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "start:alpha",
                "start:beta",
                "shutdown:beta",
                "shutdown:alpha",
            ]
        );
        assert_eq!(lifecycle.state(), LifecycleState::Terminated);
        assert_eq!(availability.state(), AvailabilityState::Broken);
    }
}
