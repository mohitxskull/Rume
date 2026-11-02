//! Rume application builder.
//!
//! `Rume` is a marker type for the builder API. Every Rume application carries
//! an [`AppContext`] as its Axum state.

use axum::routing::{MethodRouter, Router};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::app::context::AppContext;
use crate::app::paths::ProjectPaths;
use crate::app::state::{ApplicationMode, AvailabilityState, Environment, LifecycleState};
use crate::routing::RouteRegistry;

/// A configured Rume application.
///
/// `Rume` is a marker type for the builder API. Every Rume application carries
/// an [`AppContext`] as its Axum state.
pub struct Rume<E, C, S = ()> {
    _state: std::marker::PhantomData<(E, C, S)>,
}

impl<E, C, S> Rume<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Create a builder initialized with an [`AppContext`] as Axum state.
    ///
    /// This is the only public way to build a Rume application. `RumeBuilder::new`
    /// is intentionally `pub(crate)` so `Rume::builder` is the sole entry point.
    /// Every app carries an [`AppContext`] so handlers can access env, config,
    /// paths, availability, health checks, and application state through [`State`].
    pub fn builder(ctx: AppContext<E, C, S>) -> RumeBuilder<E, C, S> {
        RumeBuilder::new(ctx)
    }
}

/// Builder for a [`Rume`] application.
#[allow(clippy::type_complexity)]
pub struct RumeBuilder<E, C, S = ()>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    ctx: AppContext<E, C, S>,
    state: S,
    router: Router<AppContext<E, C, S>>,
    /// Application-scoped route registry. Cloned from the [`AppContext`] at
    /// construction; [`run`](Self::run) passes it back into the context.
    route_registry: RouteRegistry,
    bind: String,
    mode: ApplicationMode,
    environment: Environment,
    on_ready: Vec<
        Box<dyn FnOnce(AppContext<E, C, S>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>,
    >,
    on_shutdown: Vec<
        Box<dyn FnOnce(AppContext<E, C, S>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>,
    >,
    /// Named middleware registered by alias. The closure wraps the router as a
    /// Tower layer; it is applied only after `use_middleware` marks it.
    middleware: BTreeMap<
        String,
        Box<
            dyn Fn(Router<AppContext<E, C, S>>) -> Router<AppContext<E, C, S>>
                + Send
                + Sync
                + 'static,
        >,
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
    /// Maximum time to wait for all shutdown hooks before force-terminating.
    shutdown_timeout: Duration,
}

impl<E, C, S> RumeBuilder<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Construct a [`RumeBuilder`] from an [`AppContext`].
    ///
    /// Intentionally `pub(crate)`: `Rume::builder` is the only public entry
    /// point for building a Rume application.
    pub(crate) fn new(ctx: AppContext<E, C, S>) -> Self {
        let state = ctx.state.clone();
        let route_registry = ctx.route_registry.clone();
        Self {
            ctx,
            state,
            router: Router::new(),
            route_registry,
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
            shutdown_timeout: Duration::from_secs(30),
        }
    }

    /// Add a route to the application.
    ///
    /// The route is mounted on the router but not recorded in the route
    /// registry: a [`MethodRouter`] does not expose its HTTP method, so
    /// recording a [`crate::routing::RouteInfo`] here would require fabricating
    /// one (likely `GET`), which would produce incorrect `rume list:routes`
    /// output. Registration stays explicit: construct a [`RouteInfo`] from
    /// [`crate::routing::RouteSpec::build`] metadata and call
    /// [`RouteRegistry::register`] on [`AppContext::route_registry`].
    ///
    /// Why: a wrongly registered method is a real bug for `rume list:routes`,
    /// whereas an empty registry is obvious and easy to populate explicitly.
    /// Revisit if `MethodRouter` exposes its method or `route()` gains method
    /// information.
    pub fn route(mut self, path: &str, method_router: MethodRouter<AppContext<E, C, S>>) -> Self {
        self.router = self.router.route(path, method_router);
        self
    }

    /// Replace the application router.
    pub fn router(mut self, router: Router<AppContext<E, C, S>>) -> Self {
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

    /// Set the application state (db pool, redis, HTTP client, …).
    ///
    /// This is synced into the [`AppContext`] that Axum serves as state, so
    /// handlers observe it through [`State<AppContext<E, C, S>>`].
    pub fn state(mut self, state: S) -> Self {
        self.ctx.state = state.clone();
        self.state = state;
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
        F: FnOnce(AppContext<E, C, S>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_ready.push(Box::new(move |ctx| Box::pin(f(ctx))));
        self
    }

    /// Register a shutdown task.
    pub fn on_shutdown<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(AppContext<E, C, S>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_shutdown.push(Box::new(move |ctx| Box::pin(f(ctx))));
        self
    }

    /// Access the lifecycle handle.
    pub fn lifecycle(&self) -> crate::app::state::Lifecycle {
        self.ctx.lifecycle.clone()
    }

    /// Access the availability handle.
    pub fn availability(&self) -> crate::app::state::Availability {
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
        F: Fn(Router<AppContext<E, C, S>>) -> Router<AppContext<E, C, S>> + Send + Sync + 'static,
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

    /// Set the maximum time to wait for all shutdown hooks before
    /// force-terminating.
    ///
    /// A shutdown hook that exceeds this duration is aborted with a warning
    /// so a slow hook cannot block process termination indefinitely.
    pub fn shutdown_timeout(mut self, duration: Duration) -> Self {
        self.shutdown_timeout = duration;
        self
    }

    /// Build the router with all applied named middleware and groups wrapped
    /// around the routes, without binding a port. Intended for tests;
    /// [`run`](Self::run) calls this before serving.
    ///
    /// `ctx` is the Axum state the router carries; middleware closures operate
    /// on `Router<AppContext<E, C, S>>`.
    pub fn build_router(self, ctx: AppContext<E, C, S>) -> Router<AppContext<E, C, S>> {
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

        let mut router: Router<AppContext<E, C, S>> = router.with_state(ctx);

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
        // Pass the builder's route registry into the app context so the
        // application's routes are observable from `AppContext::route_registry`
        // (e.g. by `rume list:routes`).
        self.ctx.route_registry = self.route_registry.clone();

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
        // consumes `self`, so take the shutdown hooks and timeout first:
        // they must still run after the server stops.
        let on_shutdown = std::mem::take(&mut self.on_shutdown);
        let shutdown_timeout = self.shutdown_timeout;
        let serve_ctx = self.ctx.clone();
        let router = self.build_router(serve_ctx.clone());

        // `build_router` returns `Router<AppContext<E, C, S>>`; serving requires
        // `Router<()>`, so capture the state one final time at serve time.
        let result = axum::serve(listener, router.with_state(serve_ctx.clone()))
            .with_graceful_shutdown(crate::app::shutdown::shutdown_signal())
            .await;

        serve_ctx.lifecycle.set(LifecycleState::Terminating);
        serve_ctx
            .availability
            .set(AvailabilityState::RefusingTraffic);

        for hook in on_shutdown {
            match timeout(shutdown_timeout, hook(serve_ctx.clone())).await {
                Ok(()) => {}
                Err(_) => {
                    tracing::warn!(
                        "shutdown hook exceeded {}s timeout, aborting",
                        shutdown_timeout.as_secs()
                    );
                }
            }
        }

        serve_ctx.lifecycle.set(LifecycleState::Terminated);
        serve_ctx.availability.set(AvailabilityState::Unknown);

        result.map_err(|err| {
            std::io::Error::new(err.kind(), format!("server error on {addr}: {err}"))
        })
    }
}
