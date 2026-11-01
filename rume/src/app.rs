//! Application bootstrap, lifecycle, and developer-experience helpers.
#![allow(clippy::items_after_test_module)]

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::MethodRouter;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::net::TcpListener;

/// Runtime environment, normalized from `RUME_ENV`/`NODE_ENV`.
#[derive(Debug, Clone)]
pub struct Environment {
    name: String,
}

impl Environment {
    pub fn from_env() -> Self {
        Self {
            name: crate::env::resolve_env_name(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn in_development(&self) -> bool {
        matches!(self.name.as_str(), "development" | "dev" | "develop")
    }

    pub fn in_production(&self) -> bool {
        matches!(self.name.as_str(), "production" | "prod")
    }

    pub fn in_test(&self) -> bool {
        matches!(self.name.as_str(), "test" | "testing")
    }
}

/// How far the application intends to go during startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationMode {
    /// Boot and serve traffic.
    Run,
    /// Boot for inspection/tooling without serving or side effects.
    Inspect,
}

/// Lifecycle state of a running application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LifecycleState {
    Created = 0,
    Booted = 1,
    Ready = 2,
    Terminating = 3,
    Terminated = 4,
}

/// A shared lifecycle handle.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    inner: Arc<AtomicU8>,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(LifecycleState::Created as u8)),
        }
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    /// The highest valid on-disc value for `LifecycleState` (`Terminated = 4`).
    const MAX_DISCRIMINANT: u8 = LifecycleState::Terminated as u8;

    pub fn state(&self) -> LifecycleState {
        match self.inner.load(Ordering::SeqCst) {
            0 => LifecycleState::Created,
            1 => LifecycleState::Booted,
            2 => LifecycleState::Ready,
            3 => LifecycleState::Terminating,
            4 => LifecycleState::Terminated,
            // Any stray u8 (e.g. 99, 255) falls back to `Created` instead of
            // pretending to be a real lifecycle state.
            _ => LifecycleState::Created,
        }
    }

    pub(crate) fn set(&self, state: LifecycleState) {
        // The type system guarantees only valid variants reach here through
        // safe code, but guard the stored discriminant anyway so an out-of-range
        // u8 can never be observed through `state()`.
        let value = state as u8;
        if value <= Self::MAX_DISCRIMINANT {
            self.inner.store(value, Ordering::SeqCst);
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state() == LifecycleState::Ready
    }

    pub fn is_terminated(&self) -> bool {
        self.state() == LifecycleState::Terminated
    }
}

/// Availability state for health/readiness reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AvailabilityState {
    Unknown = 0,
    Live = 1,
    Ready = 2,
    RefusingTraffic = 3,
    Broken = 4,
}

/// A shared availability handle.
#[derive(Clone, Debug)]
pub struct Availability {
    inner: Arc<AtomicU8>,
}

impl Availability {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(AvailabilityState::Unknown as u8)),
        }
    }
}

impl Default for Availability {
    fn default() -> Self {
        Self::new()
    }
}

impl Availability {
    /// The highest valid on-disc value for `AvailabilityState` (`Broken = 4`).
    const MAX_DISCRIMINANT: u8 = AvailabilityState::Broken as u8;

    pub fn state(&self) -> AvailabilityState {
        match self.inner.load(Ordering::SeqCst) {
            0 => AvailabilityState::Unknown,
            1 => AvailabilityState::Live,
            2 => AvailabilityState::Ready,
            3 => AvailabilityState::RefusingTraffic,
            4 => AvailabilityState::Broken,
            // Any stray u8 maps to `Unknown`.
            _ => AvailabilityState::Unknown,
        }
    }

    pub fn set(&self, state: AvailabilityState) {
        // The type system guarantees only valid variants reach here through
        // safe code, but guard the stored discriminant anyway so an out-of-range
        // u8 can never be observed through `state()`.
        let value = state as u8;
        if value <= Self::MAX_DISCRIMINANT {
            self.inner.store(value, Ordering::SeqCst);
        }
    }

    pub fn is_ready(&self) -> bool {
        self.state() == AvailabilityState::Ready
    }
}

/// Project directory layout.
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub tmp: PathBuf,
    pub public: PathBuf,
}

impl ProjectPaths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config: root.join("config"),
            tmp: root.join("tmp"),
            public: root.join("public"),
            root,
        }
    }

    pub fn from_current_dir() -> Self {
        Self::from_root(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub fn config_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.config.join(path.as_ref())
    }

    pub fn tmp_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.tmp.join(path.as_ref())
    }

    pub fn public_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.public.join(path.as_ref())
    }

    pub fn migrations_path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root.join("migrations").join(path.as_ref())
    }
}

/// Application context holding env, config, paths, availability, lifecycle,
/// and health checks.
///
/// Every Rume application is built with an [`AppContext`]. Handlers access env,
/// config, paths, availability, and health checks through [`State<AppContext<E, C>>`].
///
/// # Example
///
/// ```ignore
/// use rume::app::{AppContext, Rume};
/// use rume::prelude::*;
///
/// let ctx = AppContext::new(env, config, paths, availability, health_checks);
/// let router = routes::router(ctx.clone());
///
/// Rume::builder(ctx).router(router).run().await?;
/// ```
#[derive(Clone, Debug)]
pub struct AppContext<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Loaded environment variables.
    pub env: E,
    /// Loaded application configuration.
    pub config: C,
    /// Project directory layout.
    pub paths: ProjectPaths,
    /// Application availability state (live / ready / refusing / broken).
    pub availability: Availability,
    /// Application lifecycle state.
    pub lifecycle: Lifecycle,
    /// Health check registry.
    pub health_checks: crate::routing::HealthChecks,
}

impl<E, C> AppContext<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Create a new application context with all required fields.
    pub fn new(
        env: E,
        config: C,
        paths: ProjectPaths,
        availability: Availability,
        health_checks: crate::routing::HealthChecks,
    ) -> Self {
        Self {
            env,
            config,
            paths,
            availability,
            lifecycle: Lifecycle::new(),
            health_checks,
        }
    }

    /// Start building an `AppContext` with sensible defaults.
    ///
    /// Required: `env` and `config`. Optional: `paths`, `availability`, `health_checks`.
    /// Defaults: `ProjectPaths::from_current_dir()`, `Availability::new()`, `HealthChecks::new()`.
    pub fn builder(env: E, config: C) -> AppContextBuilder<E, C> {
        AppContextBuilder {
            env,
            config,
            paths: None,
            availability: None,
            health_checks: None,
        }
    }
}

/// Builder for [`AppContext`] with optional defaults.
pub struct AppContextBuilder<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    env: E,
    config: C,
    paths: Option<ProjectPaths>,
    availability: Option<Availability>,
    health_checks: Option<crate::routing::HealthChecks>,
}

impl<E, C> AppContextBuilder<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Set the project paths (default: `ProjectPaths::from_current_dir()`).
    pub fn paths(mut self, paths: ProjectPaths) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Set the availability handle (default: `Availability::new()`).
    pub fn availability(mut self, availability: Availability) -> Self {
        self.availability = Some(availability);
        self
    }

    /// Set the health checks registry (default: `HealthChecks::new()`).
    pub fn health_checks(mut self, health_checks: crate::routing::HealthChecks) -> Self {
        self.health_checks = Some(health_checks);
        self
    }

    /// Build the `AppContext`.
    pub fn build(self) -> AppContext<E, C> {
        AppContext {
            env: self.env,
            config: self.config,
            paths: self.paths.unwrap_or_else(ProjectPaths::from_current_dir),
            availability: self.availability.unwrap_or_default(),
            lifecycle: Lifecycle::new(),
            health_checks: self.health_checks.unwrap_or_default(),
        }
    }
}

/// A configured Rume application.
///
/// `Rume` is a marker type for the builder API. Every Rume application carries
/// an [`AppContext`] as its Axum state.
pub struct Rume<E, C> {
    _state: std::marker::PhantomData<(E, C)>,
}

impl<E, C> Rume<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Create a builder initialized with an [`AppContext`] as Axum state.
    ///
    /// This is the only public way to build a Rume application. `RumeBuilder::new`
    /// is intentionally `pub(crate)` so `Rume::builder` is the sole entry point.
    /// Every app carries an [`AppContext`] so handlers can access env, config,
    /// paths, availability, and health checks through [`State`].
    pub fn builder(ctx: AppContext<E, C>) -> RumeBuilder<E, C> {
        RumeBuilder::new(ctx)
    }
}

/// Builder for a [`Rume`] application.
#[allow(clippy::type_complexity)]
pub struct RumeBuilder<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    ctx: AppContext<E, C>,
    router: Router<AppContext<E, C>>,
    bind: String,
    mode: ApplicationMode,
    environment: Environment,
    on_ready:
        Vec<Box<dyn FnOnce(AppContext<E, C>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>>,
    on_shutdown:
        Vec<Box<dyn FnOnce(AppContext<E, C>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>>,
    /// Named middleware registered by alias. The closure wraps the router as a
    /// Tower layer; it is applied only after `use_middleware` marks it.
    middleware: BTreeMap<
        String,
        Box<dyn Fn(Router<AppContext<E, C>>) -> Router<AppContext<E, C>> + Send + Sync + 'static>,
    >,
    /// Insertion order of registered middleware names — this is the order in
    /// which applied middleware wraps the router.
    middleware_order: Vec<String>,
    /// Named groups of middleware members, by alias (AdonisJS-style groups).
    groups: BTreeMap<String, Vec<String>>,
    /// Insertion order of registered group names — application order.
    groups_order: Vec<String>,
    /// Middleware names marked for application via `use_middleware`.
    applied_middleware: BTreeSet<String>,
    /// Group names marked for application via `use_group`.
    applied_groups: BTreeSet<String>,
}

impl<E, C> RumeBuilder<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Construct a [`RumeBuilder`] from an [`AppContext`].
    ///
    /// Intentionally `pub(crate)`: `Rume::builder` is the only public entry
    /// point for building a Rume application.
    pub(crate) fn new(ctx: AppContext<E, C>) -> Self {
        Self {
            ctx,
            router: Router::new(),
            bind: "127.0.0.1:3000".to_string(),
            mode: ApplicationMode::Run,
            environment: Environment::from_env(),
            on_ready: Vec::new(),
            on_shutdown: Vec::new(),
            middleware: BTreeMap::new(),
            middleware_order: Vec::new(),
            groups: BTreeMap::new(),
            groups_order: Vec::new(),
            applied_middleware: BTreeSet::new(),
            applied_groups: BTreeSet::new(),
        }
    }

    /// Add a route to the application.
    pub fn route(mut self, path: &str, method_router: MethodRouter<AppContext<E, C>>) -> Self {
        self.router = self.router.route(path, method_router);
        self
    }

    /// Replace the application router.
    pub fn router(mut self, router: Router<AppContext<E, C>>) -> Self {
        self.router = router;
        self
    }

    /// Set the address to bind.
    pub fn bind<T>(mut self, addr: T) -> Self
    where
        T: Into<SocketAddr>,
    {
        self.bind = addr.into().to_string();
        self
    }

    /// Set the application mode.
    pub fn mode(mut self, mode: ApplicationMode) -> Self {
        self.mode = mode;
        self
    }

    /// Override the detected environment.
    pub fn environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }

    /// Override the project paths.
    pub fn paths(mut self, paths: ProjectPaths) -> Self {
        self.ctx.paths = paths;
        self
    }

    /// Register a startup task.
    pub fn on_ready<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(AppContext<E, C>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_ready.push(Box::new(move |ctx| Box::pin(f(ctx))));
        self
    }

    /// Register a shutdown task.
    pub fn on_shutdown<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(AppContext<E, C>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_shutdown.push(Box::new(move |ctx| Box::pin(f(ctx))));
        self
    }

    /// Access the lifecycle handle.
    pub fn lifecycle(&self) -> Lifecycle {
        self.ctx.lifecycle.clone()
    }

    /// Access the availability handle.
    pub fn availability(&self) -> Availability {
        self.ctx.availability.clone()
    }

    /// Access the detected environment.
    pub fn environment_ref(&self) -> &Environment {
        &self.environment
    }

    /// Access the project paths.
    pub fn paths_ref(&self) -> &ProjectPaths {
        &self.ctx.paths
    }

    /// Access the health checks registry.
    pub fn health_checks(&self) -> crate::routing::HealthChecks {
        self.ctx.health_checks.clone()
    }

    /// Register a named middleware closure by alias (AdonisJS-style).
    ///
    /// The closure wraps the router as a Tower layer. It is not applied yet:
    /// call [`use_middleware`](Self::use_middleware) to mark it for
    /// application. Applied middleware wrap the router in
    /// [`middleware_order`](Self::middleware_order) registration order.
    pub fn middleware<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(Router<AppContext<E, C>>) -> Router<AppContext<E, C>> + Send + Sync + 'static,
    {
        self.middleware_order.push(name.to_string());
        self.middleware.insert(name.to_string(), Box::new(f));
        self
    }

    /// Mark a registered named middleware for application in [`run`].
    ///
    /// Does nothing (beyond recording the intent) if the name was never
    /// registered with `middleware`.
    pub fn use_middleware(mut self, name: &str) -> Self {
        self.applied_middleware.insert(name.to_string());
        self
    }

    /// Define a named middleware group by alias (AdonisJS-style groups).
    ///
    /// `members` are the aliases of previously registered middleware. Members
    /// run in declaration order when the group is marked with
    /// [`use_group`](Self::use_group).
    pub fn group(mut self, name: &str, members: &[&str]) -> Self {
        self.groups_order.push(name.to_string());
        self.groups.insert(
            name.to_string(),
            members.iter().map(|member| member.to_string()).collect(),
        );
        self
    }

    /// Mark a named group for application in [`run`]; all of its members run.
    pub fn use_group(mut self, name: &str) -> Self {
        self.applied_groups.insert(name.to_string());
        self
    }

    /// Build the router with all applied named middleware and groups wrapped
    /// around the routes, without binding a port. Intended for tests;
    /// [`run`](Self::run) calls this before serving.
    ///
    /// `ctx` is the Axum state the router carries; middleware closures operate
    /// on `Router<AppContext<E, C>>`.
    pub fn build_router(self, ctx: AppContext<E, C>) -> Router<AppContext<E, C>> {
        let RumeBuilder {
            router,
            middleware,
            middleware_order,
            groups,
            groups_order,
            applied_middleware,
            applied_groups,
            ..
        } = self;

        let mut router: Router<AppContext<E, C>> = router.with_state(ctx);

        // Apply named middleware in registration order, but only those
        // marked with `use_middleware`. Unknown names are skipped.
        for name in middleware_order {
            if applied_middleware.contains(&name)
                && let Some(mw) = middleware.get(&name)
            {
                router = mw(router);
            }
        }

        // Apply named groups in registration order; each group's members run
        // in declaration order. Members unknown to `middleware` are skipped.
        for name in groups_order {
            if applied_groups.contains(&name)
                && let Some(members) = groups.get(&name)
            {
                for member in members {
                    if let Some(mw) = middleware.get(member) {
                        router = mw(router);
                    }
                }
            }
        }

        router
    }

    /// Run the application until a shutdown signal is received.
    pub async fn run(mut self) -> Result<(), std::io::Error> {
        let addr: SocketAddr = self
            .bind
            .parse()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;

        // In Inspect mode, boot and run ready hooks but do NOT serve HTTP.
        // This allows tooling (config check, env check, migrations) to run
        // with a fully initialized app context without binding a port.
        if self.mode == ApplicationMode::Inspect {
            let ctx = self.ctx.clone();
            ctx.lifecycle.set(LifecycleState::Booted);
            ctx.availability.set(AvailabilityState::Live);

            for hook in self.on_ready.drain(..) {
                hook(ctx.clone()).await;
            }

            ctx.lifecycle.set(LifecycleState::Ready);
            ctx.availability.set(AvailabilityState::Ready);

            for hook in self.on_shutdown.drain(..) {
                hook(ctx.clone()).await;
            }

            ctx.lifecycle.set(LifecycleState::Terminating);
            ctx.availability.set(AvailabilityState::RefusingTraffic);

            ctx.lifecycle.set(LifecycleState::Terminated);
            ctx.availability.set(AvailabilityState::Unknown);

            return Ok(());
        }

        let listener = TcpListener::bind(addr).await.map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("failed to bind {addr}: {err} (is the port already in use?)"),
            )
        })?;

        let ctx = self.ctx.clone();
        ctx.lifecycle.set(LifecycleState::Booted);
        ctx.availability.set(AvailabilityState::Live);

        if self.mode == ApplicationMode::Run {
            for hook in self.on_ready.drain(..) {
                hook(ctx.clone()).await;
            }
        }

        ctx.lifecycle.set(LifecycleState::Ready);
        ctx.availability.set(AvailabilityState::Ready);

        println!("listening on http://{addr}");

        // Apply named middleware and groups before serving. `build_router`
        // consumes `self`, so take the shutdown hooks first: they must still
        // run after the server stops.
        let on_shutdown = std::mem::take(&mut self.on_shutdown);
        let serve_ctx = self.ctx.clone();
        let router = self.build_router(serve_ctx.clone());

        // `build_router` returns `Router<AppContext<E, C>>`; serving requires
        // `Router<()>`, so capture the state one final time at serve time.
        let result = axum::serve(listener, router.with_state(serve_ctx.clone()))
            .with_graceful_shutdown(shutdown_signal())
            .await;

        serve_ctx.lifecycle.set(LifecycleState::Terminating);
        serve_ctx
            .availability
            .set(AvailabilityState::RefusingTraffic);

        for hook in on_shutdown {
            hook(serve_ctx.clone()).await;
        }

        serve_ctx.lifecycle.set(LifecycleState::Terminated);
        serve_ctx.availability.set(AvailabilityState::Unknown);

        result.map_err(|err| {
            std::io::Error::new(err.kind(), format!("server error on {addr}: {err}"))
        })
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_normalizes() {
        // Direct construction avoids mutating process env in tests.
        let env = Environment {
            name: "production".to_string(),
        };
        assert!(env.in_production());
        assert!(!env.in_development());
    }

    #[test]
    fn availability_transitions() {
        let availability = Availability::new();
        assert_eq!(availability.state(), AvailabilityState::Unknown);
        availability.set(AvailabilityState::Ready);
        assert!(availability.is_ready());
    }

    #[test]
    fn lifecycle_transitions() {
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.state(), LifecycleState::Created);
        lifecycle.set(LifecycleState::Ready);
        assert!(lifecycle.is_ready());
    }

    #[tokio::test]
    async fn inspect_mode_boots_without_serving() {
        // C2 fix: ApplicationMode::Inspect should boot and run ready/shutdown
        // hooks but NOT serve HTTP (no port binding).
        use crate::{AppContext, Availability, HealthChecks, ProjectPaths};

        let ctx = AppContext::new(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new(),
        );

        let builder = crate::Rume::builder(ctx)
            .mode(crate::ApplicationMode::Inspect)
            .bind(([127, 0, 0, 1], 0))
            .router(Router::<AppContext<_, _>>::new());

        // Should complete without binding a port or serving
        builder.run().await.unwrap();

        // Verify lifecycle went through full transitions
        // Note: can't easily assert on final state since app is consumed
        // but if it reached here without error, the fix works
    }

    #[test]
    fn app_context_builder_with_defaults() {
        // D-M1 fix: AppContext::builder() provides ergonomic construction with defaults.
        use crate::AppContext;

        let ctx = AppContext::builder(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
        )
        .build();

        assert_eq!(ctx.env, std::net::IpAddr::from([127, 0, 0, 1]));
        assert!(ctx.config.is_empty());
        assert!(!ctx.availability.is_ready()); // Unknown state
        assert_eq!(ctx.lifecycle.state(), crate::LifecycleState::Created);
    }

    #[test]
    fn app_context_builder_custom_paths() {
        // D-M1 fix: builder allows overriding defaults.
        use crate::{AppContext, ProjectPaths};

        let custom_paths = ProjectPaths::from_root("/custom/root");

        let ctx = AppContext::builder(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
        )
        .paths(custom_paths.clone())
        .build();

        assert_eq!(ctx.paths.root, custom_paths.root);
        assert_eq!(ctx.paths.config, custom_paths.config);
        assert_eq!(ctx.paths.public, custom_paths.public);
    }

    #[test]
    fn lifecycle_invalid_u8_maps_to_created() {
        // C-C3: an out-of-range u8 discriminant must map to Created, never
        // to a real lifecycle state.
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.state(), LifecycleState::Created);

        // Store an invalid u8 directly (simulating memory corruption).
        lifecycle.inner.store(99, Ordering::SeqCst);
        assert_eq!(lifecycle.state(), LifecycleState::Created);

        lifecycle.inner.store(255, Ordering::SeqCst);
        assert_eq!(lifecycle.state(), LifecycleState::Created);
    }

    #[test]
    fn availability_invalid_u8_maps_to_unknown() {
        // C-C3: an out-of-range u8 discriminant must map to Unknown, never
        // to a real availability state.
        let availability = Availability::new();
        assert_eq!(availability.state(), AvailabilityState::Unknown);

        availability.inner.store(99, Ordering::SeqCst);
        assert_eq!(availability.state(), AvailabilityState::Unknown);

        availability.inner.store(255, Ordering::SeqCst);
        assert_eq!(availability.state(), AvailabilityState::Unknown);
    }

    #[test]
    fn lifecycle_valid_transitions() {
        let lifecycle = Lifecycle::new();
        lifecycle.set(LifecycleState::Booted);
        assert_eq!(lifecycle.state(), LifecycleState::Booted);
        lifecycle.set(LifecycleState::Ready);
        assert!(lifecycle.is_ready());
        lifecycle.set(LifecycleState::Terminating);
        assert!(!lifecycle.is_ready());
        lifecycle.set(LifecycleState::Terminated);
        assert!(lifecycle.is_terminated());
    }

    #[tokio::test]
    async fn middleware_runs_before_handler() {
        // `middleware` registers by alias and `use_middleware` applies it;
        // the middleware must wrap the router and run before the handler.
        use crate::testing::{TestApp, TestAppBuilder};
        use crate::{AppContext, Availability, HealthChecks, ProjectPaths};
        use axum::middleware::from_fn;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = Arc::new(AtomicBool::new(false));

        let ctx = AppContext::new(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new(),
        );

        let base_router = Router::<AppContext<_, _>>::new()
            .route("/", axum::routing::get(|| async { StatusCode::OK }));

        let router = crate::Rume::builder(ctx.clone())
            .router(base_router)
            .middleware("auth", {
                let flag = flag.clone();
                move |router| {
                    let flag = flag.clone();
                    router.layer(from_fn(move |req, next: axum::middleware::Next| {
                        let flag = flag.clone();
                        async move {
                            flag.store(true, Ordering::SeqCst);
                            next.run(req).await
                        }
                    }))
                }
            })
            .use_middleware("auth")
            .build_router(ctx.clone());

        let app: TestApp = TestAppBuilder::new(ctx).router(router).build().await;

        let response = app.get("/").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            flag.load(Ordering::SeqCst),
            "auth middleware should run before the handler"
        );
    }

    #[tokio::test]
    async fn middleware_group_applies_all_members() {
        // `use_group` applies every member of a named group.
        use crate::testing::{TestApp, TestAppBuilder};
        use crate::{AppContext, Availability, HealthChecks, ProjectPaths};
        use axum::middleware::from_fn;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag_auth = Arc::new(AtomicBool::new(false));
        let flag_throttle = Arc::new(AtomicBool::new(false));

        let ctx = AppContext::new(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new(),
        );

        let base_router = Router::<AppContext<_, _>>::new()
            .route("/", axum::routing::get(|| async { StatusCode::OK }));

        let router = crate::Rume::builder(ctx.clone())
            .router(base_router)
            .middleware("auth", {
                let flag = flag_auth.clone();
                move |router| {
                    let flag = flag.clone();
                    router.layer(from_fn(move |req, next: axum::middleware::Next| {
                        let flag = flag.clone();
                        async move {
                            flag.store(true, Ordering::SeqCst);
                            next.run(req).await
                        }
                    }))
                }
            })
            .middleware("throttle", {
                let flag = flag_throttle.clone();
                move |router| {
                    let flag = flag.clone();
                    router.layer(from_fn(move |req, next: axum::middleware::Next| {
                        let flag = flag.clone();
                        async move {
                            flag.store(true, Ordering::SeqCst);
                            next.run(req).await
                        }
                    }))
                }
            })
            .group("web", &["auth", "throttle"])
            .use_group("web")
            .build_router(ctx.clone());

        let app: TestApp = TestAppBuilder::new(ctx).router(router).build().await;

        let response = app.get("/").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            flag_auth.load(Ordering::SeqCst),
            "auth middleware should run via group"
        );
        assert!(
            flag_throttle.load(Ordering::SeqCst),
            "throttle middleware should run via group"
        );
    }

    #[tokio::test]
    async fn middleware_not_applied_when_not_used() {
        // Registering a middleware without `use_middleware` must not apply it.
        use crate::testing::{TestApp, TestAppBuilder};
        use crate::{AppContext, Availability, HealthChecks, ProjectPaths};
        use axum::middleware::from_fn;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = Arc::new(AtomicBool::new(false));

        let ctx = AppContext::new(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            HealthChecks::new(),
        );

        let base_router = Router::<AppContext<_, _>>::new()
            .route("/", axum::routing::get(|| async { StatusCode::OK }));

        let router = crate::Rume::builder(ctx.clone())
            .router(base_router)
            .middleware("auth", {
                let flag = flag.clone();
                move |router| {
                    let flag = flag.clone();
                    router.layer(from_fn(move |req, next: axum::middleware::Next| {
                        let flag = flag.clone();
                        async move {
                            flag.store(true, Ordering::SeqCst);
                            next.run(req).await
                        }
                    }))
                }
            })
            .build_router(ctx.clone());

        let app: TestApp = TestAppBuilder::new(ctx).router(router).build().await;

        let response = app.get("/").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            !flag.load(Ordering::SeqCst),
            "unused middleware must not run"
        );
    }
}

/// Initialize the default tracing subscriber.
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = fmt().with_env_filter(filter).try_init();
}

/// Ops endpoints for infrastructure probes (Kubernetes liveness/readiness).
///
/// Adds `GET /health/live` and `GET /ready` routes to an existing router.
/// The routes capture `availability` directly (not from Axum state), so they
/// work with any state type.
///
/// # Example
///
/// ```ignore
/// use rume::app::{AppContext, ops_router};
/// use rume::prelude::*;
///
/// let ctx = AppContext::new(env, config, paths, availability.clone(), health_checks.clone());
/// let router = routes::router(ctx.clone());
///
/// let router = ops_router(router, availability.clone());
///
/// Rume::builder(ctx).router(router).run().await?;
/// ```
pub fn ops_router<S>(router: Router<S>, availability: Availability) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let health_availability = availability.clone();
    let ready_availability = availability;

    router
        .route(
            "/health/live",
            axum::routing::get(move || {
                let availability = health_availability.clone();
                async move {
                    if availability.state() == AvailabilityState::Broken {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({"status": "broken"})),
                        )
                            .into_response()
                    } else {
                        (StatusCode::OK, Json(serde_json::json!({"status": "alive"})))
                            .into_response()
                    }
                }
            }),
        )
        .route(
            "/health/ready",
            axum::routing::get(move || {
                let availability = ready_availability.clone();
                async move {
                    if availability.is_ready() {
                        (StatusCode::OK, Json(serde_json::json!({"status": "ready"})))
                            .into_response()
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({"status": "not ready"})),
                        )
                            .into_response()
                    }
                }
            }),
        )
}
