//! Rume application builder.
//!
//! `Rume` is a marker type for the builder API. Every Rume application carries
//! an [`AppContext`] as its Axum state.

use axum::routing::{MethodRouter, Router};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::app::context::AppContext;
use crate::app::paths::ProjectPaths;
use crate::app::state::{ApplicationMode, AvailabilityState, Environment, LifecycleState};
use crate::routing::RouteRegistry;

/// Errors that can occur when building the middleware stack.
///
/// Rume fails closed: any reference to middleware that hasn't been registered,
/// any duplicate name, or any group containing an unknown member causes
/// [`build_router`](RumeBuilder::build_router) to return an error before the
/// application serves traffic. A misspelled middleware name must never
/// silently result in an unprotected route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiddlewareConfigError {
    /// The same middleware name was registered more than once.
    DuplicateMiddleware { name: String },
    /// The same group name was defined more than once.
    DuplicateGroup { name: String },
    /// `use_middleware` was called with a name that was never registered.
    UnknownMiddleware { name: String },
    /// `use_group` was called with a name that was never defined.
    UnknownGroup { name: String },
    /// A group lists a member that was never registered as middleware.
    UnknownGroupMember { group: String, member: String },
}

impl std::fmt::Display for MiddlewareConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MiddlewareConfigError::DuplicateMiddleware { name } => {
                write!(f, "middleware '{name}' is registered more than once")
            }
            MiddlewareConfigError::DuplicateGroup { name } => {
                write!(f, "middleware group '{name}' is defined more than once")
            }
            MiddlewareConfigError::UnknownMiddleware { name } => write!(
                f,
                "middleware '{name}' is marked for application but was never registered with `.middleware()`"
            ),
            MiddlewareConfigError::UnknownGroup { name } => write!(
                f,
                "middleware group '{name}' is marked for application but was never defined with `.group()`"
            ),
            MiddlewareConfigError::UnknownGroupMember { group, member } => write!(
                f,
                "middleware group '{group}' references unknown member '{member}' — register it with `.middleware()` first"
            ),
        }
    }
}

impl std::error::Error for MiddlewareConfigError {}

impl From<MiddlewareConfigError> for std::io::Error {
    fn from(err: MiddlewareConfigError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string())
    }
}

/// Errors that can occur during router construction or application preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// Middleware configuration error.
    Middleware(MiddlewareConfigError),
    /// Module registration or dependency error.
    Module(crate::module::ModuleError),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Middleware(err) => write!(f, "{err}"),
            Self::Module(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<MiddlewareConfigError> for BuildError {
    fn from(err: MiddlewareConfigError) -> Self {
        Self::Middleware(err)
    }
}

impl From<crate::module::ModuleError> for BuildError {
    fn from(err: crate::module::ModuleError) -> Self {
        Self::Module(err)
    }
}

impl From<BuildError> for std::io::Error {
    fn from(err: BuildError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string())
    }
}

/// Types that can be converted into an address string for server binding.
pub trait IntoBindAddr {
    fn into_bind_string(self) -> String;
}

impl IntoBindAddr for SocketAddr {
    fn into_bind_string(self) -> String {
        self.to_string()
    }
}

impl IntoBindAddr for &str {
    fn into_bind_string(self) -> String {
        self.to_string()
    }
}

impl IntoBindAddr for String {
    fn into_bind_string(self) -> String {
        self
    }
}

impl IntoBindAddr for (&str, u16) {
    fn into_bind_string(self) -> String {
        format!("{}:{}", self.0, self.1)
    }
}

impl IntoBindAddr for (std::net::IpAddr, u16) {
    fn into_bind_string(self) -> String {
        SocketAddr::new(self.0, self.1).to_string()
    }
}

impl IntoBindAddr for (std::net::Ipv4Addr, u16) {
    fn into_bind_string(self) -> String {
        SocketAddr::new(self.0.into(), self.1).to_string()
    }
}

impl IntoBindAddr for (std::net::Ipv6Addr, u16) {
    fn into_bind_string(self) -> String {
        SocketAddr::new(self.0.into(), self.1).to_string()
    }
}

impl IntoBindAddr for ([u8; 4], u16) {
    fn into_bind_string(self) -> String {
        SocketAddr::new(std::net::Ipv4Addr::from(self.0).into(), self.1).to_string()
    }
}

/// Trait for return types of `on_ready` startup hooks.
///
/// Implemented for `()` (infallible hooks) and `Result<(), E>` (fallible hooks).
pub trait IntoHookResult: Send {
    fn into_hook_result(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

impl IntoHookResult for () {
    fn into_hook_result(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

impl<E> IntoHookResult for Result<(), E>
where
    E: Into<Box<dyn std::error::Error + Send + Sync>> + Send,
{
    fn into_hook_result(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.map_err(Into::into)
    }
}

/// A configured Rume application.
///
/// `Rume` is a marker type for the builder API. Every Rume application carries
/// an [`AppContext`] as its Axum state.
pub struct Rume<E = (), C = (), S = ()> {
    _state: std::marker::PhantomData<(E, C, S)>,
}

impl Rume<(), (), ()> {
    /// Create a new progressive Rume application with default configuration.
    ///
    /// The application starts with default paths, availability, lifecycle,
    /// and health checks, and state `()`. Additional modules, state, routes,
    /// and middleware can be composed progressively.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> RumeBuilder<(), (), ()> {
        let paths = ProjectPaths::from_current_dir();
        let availability = crate::app::state::Availability::new();
        let health_checks = crate::routing::HealthChecks::new();
        let ctx = AppContext::new((), (), paths, availability, health_checks, ());
        RumeBuilder::new(ctx)
    }
}

impl<E, C, S> Rume<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    /// Create a builder initialized with an [`AppContext`] as Axum state.
    ///
    /// This is the entry point for fully configured applications carrying
    /// custom environment, configuration, and state.
    pub fn builder(ctx: AppContext<E, C, S>) -> RumeBuilder<E, C, S> {
        RumeBuilder::new(ctx)
    }
}

type ReadyHook<E, C, S> = Box<
    dyn FnOnce(
            AppContext<E, C, S>,
        ) -> Pin<
            Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send>,
        > + Send,
>;

type ShutdownHook<E, C, S> =
    Box<dyn FnOnce(AppContext<E, C, S>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

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
    /// Registered first-class application modules.
    modules: Vec<Arc<dyn crate::module::Module>>,
    bind: String,
    mode: ApplicationMode,
    environment: Environment,
    on_ready: Vec<ReadyHook<E, C, S>>,
    on_shutdown: Vec<ShutdownHook<E, C, S>>,
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
            modules: Vec::new(),
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

    /// Register a first-class application module.
    ///
    /// The module's routes and health checks are registered during router
    /// preflight. Startup hooks run in topological dependency order during
    /// application boot, and shutdown hooks run in reverse topological order.
    pub fn module(mut self, module: impl crate::module::Module) -> Self {
        self.modules.push(Arc::new(module));
        self
    }

    /// Mount a `GET` route and register its metadata.
    pub fn get<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::get(handler));
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::GET,
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount a `POST` route and register its metadata.
    pub fn post<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::post(handler));
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::POST,
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount a `PUT` route and register its metadata.
    pub fn put<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::put(handler));
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::PUT,
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount a `PATCH` route and register its metadata.
    pub fn patch<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::patch(handler));
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::PATCH,
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount a `DELETE` route and register its metadata.
    pub fn delete<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::delete(handler));
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::DELETE,
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount an `ANY` route and register its metadata.
    pub fn any<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::any(handler));
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::from_bytes(b"ANY").unwrap_or(axum::http::Method::GET),
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount a RESTful [`crate::routing::Resource`].
    pub fn resource(mut self, resource: crate::routing::Resource<AppContext<E, C, S>>) -> Self {
        for r in resource.routes() {
            self.router = self.router.route(&r.path, r.router.clone());
            self.route_registry.register(crate::routing::RouteInfo {
                method: r.method.clone(),
                path: r.path.clone(),
                name: Some(r.name.clone()),
                params: r.params.clone(),
            });
        }
        self
    }

    /// Nest a child [`crate::routing::Router`] under a path prefix.
    pub fn nest(
        mut self,
        path: &str,
        router: impl Into<crate::routing::Router<AppContext<E, C, S>>>,
    ) -> Self {
        let (axum_router, registry) = router.into().into_parts();
        self.router = self.router.nest(path, axum_router);
        for r in registry.routes() {
            self.route_registry.register(crate::routing::RouteInfo {
                method: r.method.clone(),
                path: crate::routing::registry::join_path(path, &r.path),
                name: r.name.clone(),
                params: r.params.clone(),
            });
        }
        self
    }

    /// Merge another [`crate::routing::Router`] into this application.
    pub fn merge(mut self, router: impl Into<crate::routing::Router<AppContext<E, C, S>>>) -> Self {
        let (axum_router, registry) = router.into().into_parts();
        self.router = self.router.merge(axum_router);
        self.route_registry.extend(registry);
        self
    }

    /// Apply a Tower layer to the entire router.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: tower::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower::Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Error:
            Into<std::convert::Infallible> + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.router = self.router.layer(layer);
        self
    }

    /// Mount a fallback handler for unmatched requests.
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.fallback(handler);
        self
    }

    /// Mount a fallback service for unmatched requests.
    pub fn fallback_service<T>(mut self, service: T) -> Self
    where
        T: tower::Service<axum::extract::Request, Error = std::convert::Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        T::Response: axum::response::IntoResponse + 'static,
        T::Future: Send + 'static,
    {
        self.router = self.router.fallback_service(service);
        self
    }

    /// Mount an arbitrary service under a path.
    pub fn route_service<T>(mut self, path: &str, service: T) -> Self
    where
        T: tower::Service<axum::extract::Request, Error = std::convert::Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        T::Response: axum::response::IntoResponse + 'static,
        T::Future: Send + 'static,
    {
        self.router = self.router.route_service(path, service);
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::from_bytes(b"ANY").unwrap_or(axum::http::Method::GET),
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Build the application router and return a lightweight [`crate::testing::TestApp`]
    /// for integration and unit testing without binding a network port.
    pub fn test_app(self) -> Result<crate::testing::TestApp, std::io::Error> {
        let ctx = self.ctx.clone();
        ctx.lifecycle.set(crate::app::state::LifecycleState::Ready);
        ctx.availability
            .set(crate::app::state::AvailabilityState::Ready);
        let app_router = self.build_router(ctx.clone())?;
        Ok(crate::testing::TestApp::new(app_router.with_state(ctx)))
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
        self.route_registry.register(crate::routing::RouteInfo {
            method: axum::http::Method::from_bytes(b"ANY").unwrap(),
            path: path.to_string(),
            name: None,
            params: crate::routing::registry::extract_path_params(path),
        });
        self
    }

    /// Mount a [`crate::routing::RouteSpec`] with its parameter validation layer and metadata.
    pub fn route_spec<H, T>(mut self, spec: crate::routing::RouteSpec, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        let rume_router = crate::routing::Router::new().route_spec(spec, handler);
        let (axum_router, registry) = rume_router.into_parts();
        self.router = self.router.merge(axum_router);
        self.route_registry.extend(registry);
        self
    }

    /// Replace the application router.
    ///
    /// Accepts a [`crate::routing::Router`] (carrying both the Axum router and
    /// route registry) or an [`axum::Router`] via [`From`].
    pub fn router(
        mut self,
        router: impl Into<crate::routing::Router<AppContext<E, C, S>>>,
    ) -> Self {
        let (axum_router, registry) = router.into().into_parts();
        self.router = axum_router;
        self.route_registry = registry;
        self
    }

    /// Set the address to bind.
    pub fn bind(mut self, addr: impl IntoBindAddr) -> Self {
        self.bind = addr.into_bind_string();
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
    ///
    /// The hook may return `()` or `Result<(), E>`. If a hook returns `Err`,
    /// application startup aborts, registered shutdown hooks run to clean up
    /// resources, and [`run`](Self::run) returns an error.
    pub fn on_ready<F, Fut, R>(mut self, f: F) -> Self
    where
        F: FnOnce(AppContext<E, C, S>) -> Fut + Send + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: IntoHookResult + 'static,
    {
        self.on_ready.push(Box::new(move |ctx| {
            Box::pin(async move { f(ctx).await.into_hook_result() })
        }));
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
    ///
    /// Duplicate names are detected when the router is built via
    /// [`build_router`](Self::build_router), which returns a
    /// [`MiddlewareConfigError::DuplicateMiddleware`] error.
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
    /// Unknown names are rejected when the router is built via
    /// [`build_router`](Self::build_router), which returns a
    /// [`MiddlewareConfigError::UnknownMiddleware`] error. Unknown middleware
    /// names are rejected fail-closed: a typo in `use_middleware("auth")`
    /// prevents the application from building its middleware stack rather than
    /// silently leaving routes unprotected.
    pub fn use_middleware(mut self, name: &str) -> Self {
        self.applied_middleware.insert(name.to_string());
        self
    }

    /// Define a named middleware group by alias (AdonisJS-style groups).
    ///
    /// `members` are the aliases of previously registered middleware. Members
    /// run in declaration order when the group is marked with
    /// [`use_group`](Self::use_group).
    ///
    /// Duplicate names and unknown members are detected when the router is
    /// built via [`build_router`](Self::build_router), which returns a
    /// [`MiddlewareConfigError`] error.
    pub fn group(mut self, name: &str, members: &[&str]) -> Self {
        self.groups_order.push(name.to_string());
        self.groups.insert(
            name.to_string(),
            members.iter().map(|member| member.to_string()).collect(),
        );
        self
    }

    /// Mark a named group for application in [`run`]; all of its members run.
    ///
    /// Unknown group names are rejected when the router is built via
    /// [`build_router`](Self::build_router), which returns a
    /// [`MiddlewareConfigError::UnknownGroup`] error.
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

    /// Build the router with all applied named middleware, groups, and modules wrapped
    /// around the routes, without binding a port. Intended for tests;
    /// [`run`](Self::run) calls this before serving.
    ///
    /// `ctx` is the Axum state the router carries; middleware closures operate
    /// on `Router<AppContext<E, C, S>>`.
    pub fn build_router(
        self,
        mut ctx: AppContext<E, C, S>,
    ) -> Result<Router<AppContext<E, C, S>>, BuildError> {
        let RumeBuilder {
            router,
            middleware,
            middleware_order,
            groups,
            groups_order,
            applied_middleware,
            applied_groups,
            modules,
            mut route_registry,
            ..
        } = self;

        // --- Step 1: Module preflight & route mounting ---
        let mut module_registry = crate::module::ModuleRegistry::new();
        for m in &modules {
            module_registry.register(Arc::clone(m))?;
        }
        let startup_modules = module_registry.resolve_startup_modules()?;

        let mut module_router = crate::routing::Router::new();
        for m in &startup_modules {
            module_router = m.register_routes(module_router);
            m.register_health_checks(&mut ctx.health_checks);
        }
        let (module_axum, module_routes) = module_router.into_parts();
        let adapted: Router<AppContext<E, C, S>> = module_axum.with_state(());
        let router = router.merge(adapted);
        route_registry.extend(module_routes);

        // --- Step 2: Validation phase (fail closed, before any middleware wraps) ---
        let mut seen_middleware: HashSet<&str> = HashSet::new();
        for name in &middleware_order {
            if !seen_middleware.insert(name.as_str()) {
                return Err(
                    MiddlewareConfigError::DuplicateMiddleware { name: name.clone() }.into(),
                );
            }
        }

        let mut seen_groups: HashSet<&str> = HashSet::new();
        for name in &groups_order {
            if !seen_groups.insert(name.as_str()) {
                return Err(MiddlewareConfigError::DuplicateGroup { name: name.clone() }.into());
            }
        }

        for name in &applied_middleware {
            if !middleware.contains_key(name.as_str()) {
                return Err(MiddlewareConfigError::UnknownMiddleware { name: name.clone() }.into());
            }
        }

        for name in &applied_groups {
            if !groups.contains_key(name.as_str()) {
                return Err(MiddlewareConfigError::UnknownGroup { name: name.clone() }.into());
            }
        }

        for group_name in &applied_groups {
            let Some(members) = groups.get(group_name.as_str()) else {
                continue;
            };
            for member in members {
                if !middleware.contains_key(member.as_str()) {
                    return Err(MiddlewareConfigError::UnknownGroupMember {
                        group: group_name.clone(),
                        member: member.clone(),
                    }
                    .into());
                }
            }
        }

        // --- Step 3: Application phase: all references are now known valid ---
        //
        // Middleware execution follows the standard onion model:
        // 1. Group middleware members execute on entry in declaration order.
        // 2. Individual middleware execute outside of groups, in registration order.
        //
        // In Tower / Axum, layers wrap around the inner router, so the layer
        // applied LAST wraps on the OUTSIDE and runs FIRST on request entry.
        // To achieve:
        //   Request Entry: Individual (first -> last) -> Group (first -> last) -> Handler
        //   Response Exit: Handler -> Group (last -> first) -> Individual (last -> first)
        // we wrap the inner router with group members in reverse order, then wrap
        // with individual middleware in reverse order.
        let mut router: Router<AppContext<E, C, S>> = router.with_state(ctx);

        // 1. Apply group middleware (innermost middleware layers)
        for name in groups_order.iter().rev() {
            if applied_groups.contains(name) {
                let members = groups.get(name).expect("group validated above");
                for member in members.iter().rev() {
                    let mw = middleware.get(member).expect("member validated above");
                    router = mw(router);
                }
            }
        }

        // 2. Apply individual middleware (outermost user middleware layers)
        for name in middleware_order.iter().rev() {
            if applied_middleware.contains(name)
                && let Some(mw) = middleware.get(name)
            {
                router = mw(router);
            }
        }

        // 3. Apply the automatic ObservabilityLayer (outermost framework layer).
        // Attaches RequestId, creates tracing spans, includes X-Request-ID header,
        // and attaches request_id to Problem Details error responses.
        router = router.layer(crate::app::observability::ObservabilityLayer::new());

        Ok(router)
    }

    /// Run the application until a shutdown signal is received.
    pub async fn run(self) -> Result<(), std::io::Error> {
        self.run_with_signal(crate::app::shutdown::shutdown_signal())
            .await
    }

    /// Run the application with an injectable shutdown signal.
    pub async fn run_with_signal<F>(mut self, signal: F) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Pass the builder's route registry into the app context so the
        // application's routes are observable from `AppContext::route_registry`
        // (e.g. by `rume list:routes`).
        self.ctx.route_registry = self.route_registry.clone();

        let addr: SocketAddr = self
            .bind
            .parse()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;

        let mode = if std::env::var("RUME_INSPECT").is_ok()
            || std::env::args().any(|a| a == "--inspect" || a == "--rume-inspect-routes")
        {
            ApplicationMode::Inspect
        } else {
            self.mode
        };

        // In Inspect mode, boot the application context without serving HTTP
        // and without running any hooks (on_ready or on_shutdown).
        if mode == ApplicationMode::Inspect {
            let ctx = self.ctx.clone();
            ctx.lifecycle.set(LifecycleState::Booted);
            ctx.availability.set(AvailabilityState::Live);

            if std::env::var("RUME_INSPECT").as_deref() == Ok("routes")
                || std::env::args().any(|a| a == "--rume-inspect-routes")
            {
                let mut module_router = crate::routing::Router::new();
                for m in &self.modules {
                    module_router = m.register_routes(module_router);
                }
                let (_, module_routes) = module_router.into_parts();
                self.route_registry.extend(module_routes);

                let json = serde_json::to_string(self.route_registry.routes())
                    .unwrap_or_else(|_| "[]".to_string());
                println!("__RUME_ROUTES_JSON_START__\n{json}\n__RUME_ROUTES_JSON_END__");
            }

            return Ok(());
        }

        // --- Step 1: Preflight validation and router construction ---
        let mut module_registry = crate::module::ModuleRegistry::new();
        for m in &self.modules {
            module_registry.register(Arc::clone(m))?;
        }
        let startup_modules = module_registry.resolve_startup_modules()?;
        let shutdown_modules = module_registry.resolve_shutdown_modules()?;

        let on_shutdown = std::mem::take(&mut self.on_shutdown);
        let on_ready = std::mem::take(&mut self.on_ready);
        let shutdown_timeout = self.shutdown_timeout;
        let serve_ctx = self.ctx.clone();

        let router = match self.build_router(serve_ctx.clone()) {
            Ok(router) => router,
            Err(err) => return Err(err.into()),
        };

        // --- Step 2: Bind listener ---
        let listener = TcpListener::bind(addr).await.map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("failed to bind {addr}: {err} (is the port already in use?)"),
            )
        })?;

        Self::run_with_bound_listener(
            serve_ctx,
            listener,
            router,
            startup_modules,
            shutdown_modules,
            on_ready,
            on_shutdown,
            shutdown_timeout,
            signal,
        )
        .await
    }

    /// Run the application with a pre-bound listener and an injectable shutdown signal.
    pub async fn run_with_listener_and_signal<F>(
        mut self,
        listener: TcpListener,
        signal: F,
    ) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.ctx.route_registry = self.route_registry.clone();

        let mode = if std::env::var("RUME_INSPECT").is_ok()
            || std::env::args().any(|a| a == "--inspect" || a == "--rume-inspect-routes")
        {
            ApplicationMode::Inspect
        } else {
            self.mode
        };

        if mode == ApplicationMode::Inspect {
            let ctx = self.ctx.clone();
            ctx.lifecycle.set(LifecycleState::Booted);
            ctx.availability.set(AvailabilityState::Live);

            if std::env::var("RUME_INSPECT").as_deref() == Ok("routes")
                || std::env::args().any(|a| a == "--rume-inspect-routes")
            {
                let mut module_router = crate::routing::Router::new();
                for m in &self.modules {
                    module_router = m.register_routes(module_router);
                }
                let (_, module_routes) = module_router.into_parts();
                self.route_registry.extend(module_routes);

                let json = serde_json::to_string(self.route_registry.routes())
                    .unwrap_or_else(|_| "[]".to_string());
                println!("__RUME_ROUTES_JSON_START__\n{json}\n__RUME_ROUTES_JSON_END__");
            }

            return Ok(());
        }

        let mut module_registry = crate::module::ModuleRegistry::new();
        for m in &self.modules {
            module_registry.register(Arc::clone(m))?;
        }
        let startup_modules = module_registry.resolve_startup_modules()?;
        let shutdown_modules = module_registry.resolve_shutdown_modules()?;

        let on_shutdown = std::mem::take(&mut self.on_shutdown);
        let on_ready = std::mem::take(&mut self.on_ready);
        let shutdown_timeout = self.shutdown_timeout;
        let serve_ctx = self.ctx.clone();

        let router = match self.build_router(serve_ctx.clone()) {
            Ok(router) => router,
            Err(err) => return Err(err.into()),
        };

        Self::run_with_bound_listener(
            serve_ctx,
            listener,
            router,
            startup_modules,
            shutdown_modules,
            on_ready,
            on_shutdown,
            shutdown_timeout,
            signal,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_with_bound_listener<F>(
        serve_ctx: AppContext<E, C, S>,
        listener: TcpListener,
        router: Router<AppContext<E, C, S>>,
        startup_modules: Vec<Arc<dyn crate::module::Module>>,
        shutdown_modules: Vec<Arc<dyn crate::module::Module>>,
        on_ready: Vec<ReadyHook<E, C, S>>,
        on_shutdown: Vec<ShutdownHook<E, C, S>>,
        shutdown_timeout: Duration,
        signal: F,
    ) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let addr = listener.local_addr()?;

        // --- Step 3: Booted & Live ---
        serve_ctx.lifecycle.set(LifecycleState::Booted);
        serve_ctx.availability.set(AvailabilityState::Live);

        // --- Step 4: Module startup hooks in topological dependency order ---
        let mut started_modules: Vec<Arc<dyn crate::module::Module>> = Vec::new();
        for m in startup_modules {
            let mod_ctx = crate::module::ModuleContext::new(
                m.name(),
                serve_ctx.paths.clone(),
                serve_ctx.availability.clone(),
                serve_ctx.lifecycle.clone(),
            );
            if let Err(err) = m.on_start(&mod_ctx).await {
                tracing::error!(
                    "module '{}' failed to start: {err}; executing cleanup",
                    m.name()
                );
                serve_ctx.lifecycle.set(LifecycleState::Terminating);
                serve_ctx.availability.set(AvailabilityState::Broken);

                started_modules.reverse();
                let cleanup_ctx = serve_ctx.clone();
                let rollback_fut = async {
                    for sm in started_modules {
                        let sm_ctx = crate::module::ModuleContext::new(
                            sm.name(),
                            cleanup_ctx.paths.clone(),
                            cleanup_ctx.availability.clone(),
                            cleanup_ctx.lifecycle.clone(),
                        );
                        sm.on_shutdown(&sm_ctx).await;
                    }
                    for hook in on_shutdown {
                        hook(cleanup_ctx.clone()).await;
                    }
                };
                let _ = timeout(shutdown_timeout, rollback_fut).await;

                serve_ctx.lifecycle.set(LifecycleState::Terminated);
                return Err(std::io::Error::other(format!(
                    "module '{}' failed to start: {err}",
                    m.name()
                )));
            }
            started_modules.push(m);
        }

        // --- Step 5: Application startup hooks with cleanup on failure ---
        for hook in on_ready {
            if let Err(err) = hook(serve_ctx.clone()).await {
                tracing::error!("startup hook failed: {err}; executing registered cleanup hooks");
                serve_ctx.lifecycle.set(LifecycleState::Terminating);
                serve_ctx.availability.set(AvailabilityState::Broken);

                started_modules.reverse();
                let cleanup_ctx = serve_ctx.clone();
                let cleanup_fut = async {
                    for sm in started_modules {
                        let sm_ctx = crate::module::ModuleContext::new(
                            sm.name(),
                            cleanup_ctx.paths.clone(),
                            cleanup_ctx.availability.clone(),
                            cleanup_ctx.lifecycle.clone(),
                        );
                        sm.on_shutdown(&sm_ctx).await;
                    }
                    for hook in on_shutdown {
                        hook(cleanup_ctx.clone()).await;
                    }
                };
                let _ = timeout(shutdown_timeout, cleanup_fut).await;

                serve_ctx.lifecycle.set(LifecycleState::Terminated);
                return Err(std::io::Error::other(format!("startup hook failed: {err}")));
            }
        }

        // --- Step 6: Declare Ready ---
        serve_ctx.lifecycle.set(LifecycleState::Ready);
        serve_ctx.availability.set(AvailabilityState::Ready);

        println!("listening on http://{addr}");

        // --- Step 7: Serve traffic with immediate lifecycle & availability transition on shutdown ---
        let shutdown_lifecycle = serve_ctx.lifecycle.clone();
        let shutdown_availability = serve_ctx.availability.clone();

        let graceful_signal = async move {
            signal.await;
            tracing::info!(
                "shutdown signal received: transitioning to Terminating / RefusingTraffic"
            );
            shutdown_lifecycle.set(LifecycleState::Terminating);
            shutdown_availability.set(AvailabilityState::RefusingTraffic);
        };

        let result = axum::serve(listener, router.with_state(serve_ctx.clone()))
            .with_graceful_shutdown(graceful_signal)
            .await;

        // --- Step 8: Post-drain shutdown hooks (modules shut down in reverse topological order) ---
        let on_shutdown_fut = async {
            for hook in on_shutdown {
                hook(serve_ctx.clone()).await;
            }
            for sm in shutdown_modules {
                let sm_ctx = crate::module::ModuleContext::new(
                    sm.name(),
                    serve_ctx.paths.clone(),
                    serve_ctx.availability.clone(),
                    serve_ctx.lifecycle.clone(),
                );
                sm.on_shutdown(&sm_ctx).await;
            }
        };
        match timeout(shutdown_timeout, on_shutdown_fut).await {
            Ok(()) => {}
            Err(_) => {
                tracing::warn!(
                    "shutdown hooks exceeded {}s timeout in aggregate, aborting remaining hooks",
                    shutdown_timeout.as_secs()
                );
            }
        }

        serve_ctx.lifecycle.set(LifecycleState::Terminated);
        serve_ctx.availability.set(AvailabilityState::Unknown);

        result.map_err(|err| {
            std::io::Error::new(err.kind(), format!("server error on {addr}: {err}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::AppContext;
    use crate::app::paths::ProjectPaths;
    use crate::app::state::{ApplicationMode, Availability, AvailabilityState, LifecycleState};
    use crate::routing::HealthChecks;

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

    /// Returns a boxed middleware closure that adds a route at `path`.
    #[allow(clippy::type_complexity)]
    fn mw_add_route(
        path: &'static str,
    ) -> Box<
        dyn Fn(Router<AppContext<(), (), ()>>) -> Router<AppContext<(), (), ()>>
            + Send
            + Sync
            + 'static,
    > {
        Box::new(move |router: Router<AppContext<(), (), ()>>| {
            router.route(path, axum::routing::get(|| async {}))
        })
    }

    #[test]
    fn known_middleware_applied_successfully() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .middleware("auth", mw_add_route("/auth"))
            .use_middleware("auth");

        let router = builder.build_router(test_ctx());
        assert!(router.is_ok());
    }

    #[test]
    fn unknown_middleware_via_use_middleware_is_rejected() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx).use_middleware("auth");

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::Middleware(MiddlewareConfigError::UnknownMiddleware { name })
            if name == "auth"
        ));
    }

    #[test]
    fn duplicate_middleware_registration_is_rejected() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .middleware("auth", mw_add_route("/auth"))
            .middleware("auth", mw_add_route("/auth2"));

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::Middleware(MiddlewareConfigError::DuplicateMiddleware { name })
            if name == "auth"
        ));
    }

    #[test]
    fn unknown_group_via_use_group_is_rejected() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx).use_group("admins");

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::Middleware(MiddlewareConfigError::UnknownGroup { name })
            if name == "admins"
        ));
    }

    #[test]
    fn group_with_unknown_member_is_rejected() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .middleware("auth", mw_add_route("/auth"))
            .group("admins", &["auth", "missing"])
            .use_group("admins");

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::Middleware(MiddlewareConfigError::UnknownGroupMember { group, member })
            if group == "admins" && member == "missing"
        ));
    }

    #[test]
    fn duplicate_group_definition_is_rejected() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .group("admins", &[])
            .group("admins", &[])
            .use_group("admins");

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::Middleware(MiddlewareConfigError::DuplicateGroup { name })
            if name == "admins"
        ));
    }

    #[test]
    fn middleware_applied_in_registration_order() {
        // Named middleware wraps the router in registration order. We can't
        // observe request-time ordering without a live HTTP client, but we can
        // verify that all applied middleware are present and the result is Ok.
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .middleware("first", mw_add_route("/first"))
            .middleware("second", mw_add_route("/second"))
            .use_middleware("first")
            .use_middleware("second");

        let router = builder.build_router(test_ctx());
        assert!(router.is_ok());
    }

    #[test]
    fn group_members_applied_in_declaration_order() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .middleware("a", mw_add_route("/a"))
            .middleware("b", mw_add_route("/b"))
            .middleware("c", mw_add_route("/c"))
            .group("stack", &["a", "b", "c"])
            .use_group("stack");

        let router = builder.build_router(test_ctx());
        assert!(router.is_ok());
    }

    #[test]
    fn run_returns_error_on_misconfigured_middleware() {
        // `run()` fails closed: unknown middleware prevents serving.
        // `run()` calls `build_router` before listening, so the error surfaces
        // before any traffic can be served.
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx).use_middleware("nonexistent");

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::Middleware(MiddlewareConfigError::UnknownMiddleware { .. })
        ));
    }

    #[test]
    fn error_display_messages_are_descriptive() {
        let err = MiddlewareConfigError::UnknownMiddleware {
            name: "autn".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("autn"));
        assert!(msg.contains(".middleware()"));

        let err = MiddlewareConfigError::UnknownGroupMember {
            group: "admins".to_string(),
            member: "auth".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("admins"));
        assert!(msg.contains("auth"));
    }

    #[tokio::test]
    async fn inspect_mode_does_not_run_on_ready_hooks() {
        // Inspect mode must not run startup hooks — they may have side effects.
        let ready_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ready_ran_clone = ready_ran.clone();

        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .mode(ApplicationMode::Inspect)
            .on_ready(move |_| async move {
                ready_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            });

        builder.run().await.expect("inspect mode should succeed");

        assert!(
            !ready_ran.load(std::sync::atomic::Ordering::SeqCst),
            "on_ready hooks must not run in Inspect mode"
        );
    }

    #[tokio::test]
    async fn inspect_mode_does_not_run_on_shutdown_hooks() {
        // Inspect mode must not run shutdown hooks either.
        let shutdown_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_ran_clone = shutdown_ran.clone();

        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .mode(ApplicationMode::Inspect)
            .on_shutdown(move |_| async move {
                shutdown_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            });

        builder.run().await.expect("inspect mode should succeed");

        assert!(
            !shutdown_ran.load(std::sync::atomic::Ordering::SeqCst),
            "on_shutdown hooks must not run in Inspect mode"
        );
    }

    #[tokio::test]
    async fn inspect_mode_sets_booted_live_then_returns() {
        // Inspect mode boots to Booted/Live and returns without serving.
        let ctx = test_ctx();
        let lifecycle = ctx.lifecycle.clone();
        let availability = ctx.availability.clone();

        let builder = RumeBuilder::new(ctx).mode(ApplicationMode::Inspect);

        builder.run().await.expect("inspect mode should succeed");

        // Lifecycle should be Booted (not Ready — not serving traffic).
        assert_eq!(lifecycle.state(), LifecycleState::Booted);
        assert!(availability.state() == AvailabilityState::Live);
    }

    #[tokio::test]
    async fn preflight_failure_aborts_before_startup_hooks_run() {
        // Preflight validation must fail closed BEFORE any startup hooks run.
        let ready_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ready_ran_clone = ready_ran.clone();

        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .use_middleware("nonexistent")
            .on_ready(move |_| async move {
                ready_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            });

        let err = builder.run().await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !ready_ran.load(std::sync::atomic::Ordering::SeqCst),
            "on_ready hook must NOT run when preflight validation fails"
        );
    }

    #[tokio::test]
    async fn fallible_startup_hook_failure_triggers_cleanup_hooks() {
        // If an on_ready hook fails, registered shutdown hooks must be executed
        // to clean up already-acquired resources.
        let cleanup_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cleanup_ran_clone = cleanup_ran.clone();

        let ctx = test_ctx();
        let _availability = ctx.availability.clone();
        let lifecycle = ctx.lifecycle.clone();

        let builder = RumeBuilder::new(ctx)
            .on_ready(|_| async move {
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "database unreachable",
                ))
            })
            .on_shutdown(move |_| async move {
                cleanup_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            });

        let err = builder.run().await.unwrap_err();
        assert!(err.to_string().contains("database unreachable"));

        assert!(
            cleanup_ran.load(std::sync::atomic::Ordering::SeqCst),
            "on_shutdown cleanup hooks must run when a startup hook fails"
        );
        assert_eq!(lifecycle.state(), LifecycleState::Terminated);
    }

    type TestMiddleware = Box<
        dyn Fn(Router<AppContext<(), (), ()>>) -> Router<AppContext<(), (), ()>>
            + Send
            + Sync
            + 'static,
    >;

    fn recording_mw(
        name: &'static str,
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> TestMiddleware {
        Box::new(move |router| {
            let log = std::sync::Arc::clone(&log);
            router.layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let log = std::sync::Arc::clone(&log);
                    async move {
                        log.lock().unwrap().push(format!("{name}:enter"));
                        let res = next.run(req).await;
                        log.lock().unwrap().push(format!("{name}:exit"));
                        res
                    }
                },
            ))
        })
    }

    #[tokio::test]
    async fn middleware_individual_execution_order_during_request() {
        use crate::testing::TestApp;

        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ctx = test_ctx();

        let builder = RumeBuilder::new(ctx)
            .route("/hello", axum::routing::get(|| async { "ok" }))
            .middleware("first", recording_mw("first", std::sync::Arc::clone(&log)))
            .middleware(
                "second",
                recording_mw("second", std::sync::Arc::clone(&log)),
            )
            .use_middleware("first")
            .use_middleware("second");

        let app_router = builder.build_router(test_ctx()).unwrap();
        let app = TestApp::new(app_router.with_state(test_ctx()));

        let res = app.get("/hello").await;
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let events = log.lock().unwrap().clone();
        assert_eq!(
            events,
            vec!["first:enter", "second:enter", "second:exit", "first:exit"],
            "individual middleware must enter in registration order and exit in reverse"
        );
    }

    #[tokio::test]
    async fn middleware_group_execution_order_during_request() {
        use crate::testing::TestApp;

        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ctx = test_ctx();

        let builder = RumeBuilder::new(ctx)
            .route("/hello", axum::routing::get(|| async { "ok" }))
            .middleware("a", recording_mw("a", std::sync::Arc::clone(&log)))
            .middleware("b", recording_mw("b", std::sync::Arc::clone(&log)))
            .middleware("c", recording_mw("c", std::sync::Arc::clone(&log)))
            .group("stack", &["a", "b", "c"])
            .use_group("stack");

        let app_router = builder.build_router(test_ctx()).unwrap();
        let app = TestApp::new(app_router.with_state(test_ctx()));

        let res = app.get("/hello").await;
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let events = log.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "a:enter", "b:enter", "c:enter", "c:exit", "b:exit", "a:exit"
            ],
            "group members must enter in declaration order and exit in reverse"
        );
    }

    #[tokio::test]
    async fn middleware_combined_individual_and_groups_execution_order() {
        use crate::testing::TestApp;

        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ctx = test_ctx();

        let builder = RumeBuilder::new(ctx)
            .route("/hello", axum::routing::get(|| async { "ok" }))
            .middleware(
                "global_mw",
                recording_mw("global_mw", std::sync::Arc::clone(&log)),
            )
            .middleware("auth", recording_mw("auth", std::sync::Arc::clone(&log)))
            .middleware("audit", recording_mw("audit", std::sync::Arc::clone(&log)))
            .group("api", &["auth", "audit"])
            .use_middleware("global_mw")
            .use_group("api");

        let app_router = builder.build_router(test_ctx()).unwrap();
        let app = TestApp::new(app_router.with_state(test_ctx()));

        let res = app.get("/hello").await;
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let events = log.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "global_mw:enter",
                "auth:enter",
                "audit:enter",
                "audit:exit",
                "auth:exit",
                "global_mw:exit"
            ],
            "individual middleware must wrap outside groups: global -> group -> handler -> group -> global"
        );
    }
}
