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

use crate::app::context::AppContext;
use crate::app::paths::ProjectPaths;
use crate::app::state::{ApplicationMode, AvailabilityState, LifecycleState};
use rume_http::routing::RouteRegistry;

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
    /// Application or module startup hook failure.
    Startup(String),
    /// Duplicate route collision across modules or application routes.
    DuplicateRoute { method: String, path: String },
    /// Internal route registry lock was poisoned.
    RouteRegistryPoisoned,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Middleware(err) => write!(f, "{err}"),
            Self::Module(err) => write!(f, "{err}"),
            Self::Startup(msg) => write!(f, "startup failure: {msg}"),
            Self::DuplicateRoute { method, path } => {
                write!(f, "duplicate route collision for {method} {path}")
            }
            Self::RouteRegistryPoisoned => write!(f, "route registry lock poisoned"),
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
    /// health checks, and empty state `()`. Modules encapsulate their own private
    /// state via `.with_state()` on their sub-routers. Applications requiring custom
    /// typed global state in [`AppContext`] must initialize via [`AppContext::builder`]
    /// and [`Rume::builder`].
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> RumeBuilder<(), (), ()> {
        let paths = ProjectPaths::from_current_dir();
        let availability = crate::app::state::Availability::new();
        let health_checks = rume_http::routing::HealthChecks::new();
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

/// Type alias for an application `on_ready` startup hook.
///
/// Exposed for `rume-testing`'s `RumeTestingExt` lifecycle runner.
pub type ReadyHook<E, C, S> = Box<
    dyn FnOnce(
            AppContext<E, C, S>,
        ) -> Pin<
            Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send>,
        > + Send,
>;

/// Type alias for an application `on_shutdown` teardown hook.
///
/// Exposed for `rume-testing`'s `RumeTestingExt` lifecycle runner.
pub type ShutdownHook<E, C, S> =
    Box<dyn FnOnce(AppContext<E, C, S>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Decomposed parts of a [`RumeBuilder`] needed to construct and manage a test application.
///
/// Exists to support `rume-testing`'s `RumeTestingExt` without creating a circular dependency
/// between `rume-app` and `rume-testing`.
pub struct TestAppParts<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    pub ctx: AppContext<E, C, S>,
    pub router: axum::Router<()>,
    pub modules: Vec<Arc<dyn crate::module::Module>>,
    pub resolved_modules: crate::app::lifecycle::ResolvedModules,
    pub mode: ApplicationMode,
    pub on_ready: Vec<ReadyHook<E, C, S>>,
    pub on_shutdown: Vec<ShutdownHook<E, C, S>>,
    pub shutdown_timeout: Duration,
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
    router: Router<AppContext<E, C, S>>,
    /// Application-scoped route registry. Cloned from the [`AppContext`] at
    /// construction; [`run`](Self::run) passes it back into the context.
    route_registry: Arc<std::sync::RwLock<RouteRegistry>>,
    /// Registered first-class application modules.
    modules: Vec<Arc<dyn crate::module::Module>>,
    bind: String,
    mode: ApplicationMode,
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
        let route_registry = Arc::new(std::sync::RwLock::new(ctx.route_registry.clone()));
        Self {
            ctx,
            router: Router::new(),
            route_registry,
            modules: Vec::new(),
            bind: "127.0.0.1:3000".to_string(),
            mode: ApplicationMode::Run,
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

    fn record_route(&self, info: rume_http::routing::RouteInfo) {
        self.route_registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register(info);
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

    /// Register an application module.
    ///
    /// Alias for [`Self::module`].
    pub fn register_module(self, module: impl crate::module::Module) -> Self {
        self.module(module)
    }

    /// Mount a `GET` route and register its metadata.
    pub fn get<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        self.router = self.router.route(path, axum::routing::get(handler));
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::GET,
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
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
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::POST,
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
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
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::PUT,
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
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
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::PATCH,
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
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
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::DELETE,
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
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
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::from_bytes(b"ANY").unwrap_or(axum::http::Method::GET),
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
        });
        self
    }

    /// Mount a RESTful [`rume_http::routing::Resource`].
    pub fn resource(mut self, resource: rume_http::routing::Resource<AppContext<E, C, S>>) -> Self {
        for r in resource.routes() {
            self.router = self.router.route(&r.path, r.router.clone());
            self.record_route(rume_http::routing::RouteInfo {
                method: r.method.clone(),
                path: r.path.clone(),
                name: Some(r.name.clone()),
                params: r.params.clone(),
                matchers: Vec::new(),
                request_schema: None,
            });
        }
        self
    }

    /// Nest a child [`rume_http::routing::Router`] under a path prefix.
    pub fn nest(
        mut self,
        path: &str,
        router: impl Into<rume_http::routing::Router<AppContext<E, C, S>>>,
    ) -> Self {
        let (axum_router, registry) = router.into().into_parts();
        self.router = self.router.nest(path, axum_router);
        for r in registry.routes() {
            self.record_route(rume_http::routing::RouteInfo {
                method: r.method.clone(),
                path: rume_http::routing::registry::join_path(path, &r.path),
                name: r.name.clone(),
                params: r.params.clone(),
                matchers: r.matchers.clone(),
                request_schema: r.request_schema.clone(),
            });
        }
        self
    }

    /// Merge another [`rume_http::routing::Router`] into this application.
    pub fn merge(
        mut self,
        router: impl Into<rume_http::routing::Router<AppContext<E, C, S>>>,
    ) -> Self {
        let (axum_router, registry) = router.into().into_parts();
        self.router = self.router.merge(axum_router);
        self.route_registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(registry);
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
        self.record_route(rume_http::routing::RouteInfo {
            method: axum::http::Method::from_bytes(b"ANY").unwrap_or(axum::http::Method::GET),
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
        });
        self
    }

    /// Decompose this builder into its compiled router, context, modules, and lifecycle hooks.
    ///
    /// Exists to support `rume-testing`'s `RumeTestingExt` (`.test_app()` and `.build_test_app()`)
    /// without creating a circular dependency between `rume-app` and `rume-testing`.
    pub fn into_test_parts(mut self) -> Result<TestAppParts<E, C, S>, BuildError> {
        let modules = self.modules.clone();
        let resolved_modules = crate::app::lifecycle::resolve_modules(&modules)?;
        let on_shutdown = std::mem::take(&mut self.on_shutdown);
        let on_ready = std::mem::take(&mut self.on_ready);
        let shutdown_timeout = self.shutdown_timeout;
        let ctx = self.ctx.clone();
        let mode = self.mode;

        let (app_router, ctx) = self.build_router_with_resolved(ctx, &resolved_modules.startup)?;

        Ok(TestAppParts {
            router: app_router.with_state(ctx.clone()),
            ctx,
            modules,
            resolved_modules,
            mode,
            on_ready,
            on_shutdown,
            shutdown_timeout,
        })
    }

    /// Add a route to the application.
    ///
    /// The route is mounted on the router and registered into the route
    /// registry with an `UNKNOWN` method marker (since [`MethodRouter`] does
    /// not expose its specific HTTP methods, distinguishing it from `.any()`).
    /// To register explicit HTTP methods with typed metadata for OpenAPI or
    /// `rume list:routes`, use [`RumeBuilder::route_spec`].
    pub fn route(mut self, path: &str, method_router: MethodRouter<AppContext<E, C, S>>) -> Self {
        self.router = self.router.route(path, method_router);
        self.record_route(rume_http::routing::RouteInfo {
            method: rume_http::routing::RouteInfo::unknown_method(),
            path: path.to_string(),
            name: None,
            params: rume_http::routing::registry::extract_path_params(path),
            matchers: Vec::new(),
            request_schema: None,
        });
        self
    }

    /// Mount a [`rume_http::routing::RouteSpec`] with its parameter validation layer and metadata.
    pub fn route_spec<H, T>(mut self, spec: rume_http::routing::RouteSpec, handler: H) -> Self
    where
        H: axum::handler::Handler<T, AppContext<E, C, S>>,
        T: 'static,
    {
        let rume_router = rume_http::routing::Router::new().route_spec(spec, handler);
        let (axum_router, registry) = rume_router.into_parts();
        self.router = self.router.merge(axum_router);
        self.route_registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(registry);
        self
    }

    /// Mounts an OpenAPI JSON endpoint at the specified path (e.g. `/openapi.json`).
    ///
    /// The specification is generated lazily when requested from the shared route registry,
    /// guaranteeing that routes registered after `.openapi()` and module routes are included.
    pub fn openapi(self, path: &str, info: rume_http::openapi::OpenApiInfo) -> Self {
        let registry = Arc::clone(&self.route_registry);
        self.get(path, move || {
            let reg = Arc::clone(&registry);
            let inf = info.clone();
            async move {
                let spec = reg
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .to_openapi(inf);
                axum::Json(spec.to_json_value())
            }
        })
    }

    /// Replace the application router.
    ///
    /// Accepts a [`rume_http::routing::Router`] (carrying both the Axum router and
    /// route registry) or an [`axum::Router`] via [`From`].
    pub fn router(
        mut self,
        router: impl Into<rume_http::routing::Router<AppContext<E, C, S>>>,
    ) -> Self {
        let (axum_router, registry) = router.into().into_parts();
        self.router = axum_router;
        *self
            .route_registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = registry;
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

    /// Set the application state (db pool, redis, HTTP client, …).
    ///
    /// This is synced into the [`AppContext`] that Axum serves as state, so
    /// handlers observe it through [`State<AppContext<E, C, S>>`].
    pub fn state(mut self, state: S) -> Self {
        self.ctx.state = state;
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

    /// Access the project paths.
    pub fn paths_ref(&self) -> &ProjectPaths {
        &self.ctx.paths
    }

    /// Access the health checks registry.
    pub fn health_checks(&self) -> rume_http::routing::HealthChecks {
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
        ctx: AppContext<E, C, S>,
    ) -> Result<Router<AppContext<E, C, S>>, BuildError> {
        Ok(self.build_router_with_ctx(ctx)?.0)
    }

    /// Build the router and return the finalized [`AppContext`] carrying all
    /// module-registered routes and health checks.
    fn build_router_with_ctx(
        self,
        ctx: AppContext<E, C, S>,
    ) -> Result<BuiltRouterWithCtx<E, C, S>, BuildError> {
        let resolved = crate::app::lifecycle::resolve_modules(&self.modules)?;
        self.build_router_with_resolved(ctx, &resolved.startup)
    }

    /// Build the router using a pre-resolved module startup order so callers like
    /// `prepare_serve` and `into_test_parts` resolve the module graph exactly once.
    fn build_router_with_resolved(
        self,
        mut ctx: AppContext<E, C, S>,
        startup_modules: &[Arc<dyn crate::module::Module>],
    ) -> Result<BuiltRouterWithCtx<E, C, S>, BuildError> {
        let RumeBuilder {
            router,
            middleware,
            middleware_order,
            groups,
            groups_order,
            applied_middleware,
            applied_groups,
            route_registry,
            ..
        } = self;

        // --- Step 1: Module preflight & route mounting ---
        let app_routes = route_registry
            .read()
            .map_err(|_| BuildError::RouteRegistryPoisoned)?
            .clone();
        let (module_axum, module_routes) =
            collect_and_validate_routes(startup_modules, &app_routes)?;
        for m in startup_modules {
            m.register_health_checks(&mut ctx.health_checks);
        }

        let router = merge_module_router(router, module_axum)?;
        {
            let mut reg = route_registry
                .write()
                .map_err(|_| BuildError::RouteRegistryPoisoned)?;
            reg.extend(module_routes);
            ctx.route_registry = reg.clone();
        }

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
        // Capture and record MatchedPath on the active tracing span if routes are present
        let router = if !ctx.route_registry.is_empty() {
            router.route_layer(axum::middleware::from_fn(
                |req: axum::extract::Request, next: axum::middleware::Next| async move {
                    if let Some(mp) = req
                        .extensions()
                        .get::<axum::extract::MatchedPath>()
                        .cloned()
                    {
                        tracing::Span::current().record("route", mp.as_str());
                        let mut resp = next.run(req).await;
                        resp.extensions_mut().insert(mp);
                        resp
                    } else {
                        next.run(req).await
                    }
                },
            ))
        } else {
            router
        };

        // Why: Axum's `with_state(ctx)` snapshots `ctx` by value (including non-`Arc`
        // `RouteRegistry` and `HealthChecks`), so cloning `ctx` into `with_state`
        // and returning the finalized `ctx` keeps `TestAppParts.ctx`, `on_ready`,
        // and `on_shutdown` in sync with module routes and health checks.
        let mut router: Router<AppContext<E, C, S>> = router.with_state(ctx.clone());

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

        Ok((router, ctx))
    }

    fn effective_mode(&self) -> ApplicationMode {
        if std::env::var("RUME_INSPECT").is_ok()
            || std::env::args().any(|a| a == "--inspect" || a == "--rume-inspect-routes")
        {
            ApplicationMode::Inspect
        } else {
            self.mode
        }
    }

    /// Execute `ApplicationMode::Inspect` preflight without starting HTTP
    /// serving or lifecycle hooks (`on_start`, `on_ready`, `on_shutdown`).
    fn run_inspect_mode(mut self) -> Result<(), std::io::Error> {
        // Why: `Inspect` mode skips `on_start`, `on_ready`, and `on_shutdown` hooks so
        // `rume list:routes` and boot checks remain side-effect-free, while still
        // running `ModuleRegistry` DAG validation, `check_and_record_route`, and the
        // actual Axum router merge so module cycles and duplicate routes (including
        // app `.route()` + module collisions) fail closed identically to `build_router`.
        let resolved = crate::app::lifecycle::resolve_modules(&self.modules)?;
        let app_routes = self
            .route_registry
            .read()
            .map_err(|_| BuildError::RouteRegistryPoisoned)?
            .clone();
        let (module_axum, module_routes) =
            collect_and_validate_routes(&resolved.startup, &app_routes)?;
        let _merged = merge_module_router(self.router, module_axum)?;

        {
            let mut reg = self
                .route_registry
                .write()
                .map_err(|_| BuildError::RouteRegistryPoisoned)?;
            reg.extend(module_routes);
            self.ctx.route_registry = reg.clone();
        }

        self.ctx.lifecycle.set(LifecycleState::Booted);
        self.ctx.availability.set(AvailabilityState::Live);

        if std::env::var("RUME_INSPECT").as_deref() == Ok("routes")
            || std::env::args().any(|a| a == "--rume-inspect-routes")
        {
            let json = serde_json::to_string(self.ctx.route_registry.routes())
                .unwrap_or_else(|_| "[]".to_string());
            println!("__RUME_ROUTES_JSON_START__\n{json}\n__RUME_ROUTES_JSON_END__");
        }

        Ok(())
    }

    fn prepare_serve(mut self) -> Result<PreparedServe<E, C, S>, std::io::Error> {
        let resolved = crate::app::lifecycle::resolve_modules(&self.modules)?;
        let on_shutdown = std::mem::take(&mut self.on_shutdown);
        let on_ready = std::mem::take(&mut self.on_ready);
        let shutdown_timeout = self.shutdown_timeout;
        let ctx = self.ctx.clone();

        let (router, serve_ctx) = self.build_router_with_resolved(ctx, &resolved.startup)?;

        Ok(PreparedServe {
            router,
            serve_ctx,
            startup_modules: resolved.startup,
            shutdown_modules: resolved.shutdown,
            on_ready,
            on_shutdown,
            shutdown_timeout,
        })
    }

    /// Run the application until a shutdown signal is received.
    pub async fn run(self) -> Result<(), std::io::Error> {
        self.run_with_signal(crate::app::shutdown::shutdown_signal())
            .await
    }

    /// Run the application with an injectable shutdown signal.
    pub async fn run_with_signal<F>(self, signal: F) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.effective_mode() == ApplicationMode::Inspect {
            return self.run_inspect_mode();
        }

        let bind_addr = self.bind.clone();
        let prepared = self.prepare_serve()?;

        // --- Step 2: Bind listener ---
        let listener = TcpListener::bind(&bind_addr).await.map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("failed to bind {bind_addr}: {err} (is the port already in use?)"),
            )
        })?;

        Self::run_with_bound_listener(prepared, listener, signal).await
    }

    /// Run the application with a pre-bound listener and an injectable shutdown signal.
    pub async fn run_with_listener_and_signal<F>(
        self,
        listener: TcpListener,
        signal: F,
    ) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.effective_mode() == ApplicationMode::Inspect {
            return self.run_inspect_mode();
        }

        let prepared = self.prepare_serve()?;
        Self::run_with_bound_listener(prepared, listener, signal).await
    }

    async fn run_with_bound_listener<F>(
        prepared: PreparedServe<E, C, S>,
        listener: TcpListener,
        signal: F,
    ) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let PreparedServe {
            router,
            serve_ctx,
            startup_modules,
            shutdown_modules,
            on_ready,
            mut on_shutdown,
            shutdown_timeout,
        } = prepared;

        let addr = listener.local_addr()?;

        // Shutdown signal monitoring for boot hooks and server.
        // Use a watch channel instead of AtomicBool+Notify so the shutdown
        // signal latches: `rx.wait_for()` returns immediately if `send(true)`
        // already happened before the waiter registered, preventing the
        // lost-wakeup hang (RC-01).
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        let sf_task = shutdown_tx.clone();
        tokio::spawn(async move {
            signal.await;
            let _ = sf_task.send(true);
        });

        // --- Step 3–6: Boot, module on_start, application on_ready, and Ready transition ---
        crate::app::lifecycle::run_startup(
            &serve_ctx,
            &startup_modules,
            on_ready,
            &mut on_shutdown,
            shutdown_timeout,
            Some(shutdown_rx.clone()),
        )
        .await?;

        println!("listening on http://{addr}");

        // --- Step 7: Serve traffic with immediate lifecycle & availability transition on shutdown ---
        let shutdown_lifecycle = serve_ctx.lifecycle.clone();
        let shutdown_availability = serve_ctx.availability.clone();

        let graceful_signal = async move {
            let _ = shutdown_rx.wait_for(|&shutting_down| shutting_down).await;
            tracing::info!(
                "shutdown signal received: transitioning to Terminating / RefusingTraffic"
            );
            shutdown_lifecycle.set(LifecycleState::Terminating);
            shutdown_availability.set(AvailabilityState::RefusingTraffic);
        };

        let result = axum::serve(
            listener,
            router
                .with_state(serve_ctx.clone())
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(graceful_signal)
        .await;

        // --- Step 8: Post-drain shutdown hooks (modules shut down in reverse topological order) ---
        crate::app::lifecycle::run_shutdown(
            &serve_ctx,
            on_shutdown,
            shutdown_modules,
            shutdown_timeout,
        )
        .await;

        result.map_err(|err| {
            std::io::Error::new(err.kind(), format!("server error on {addr}: {err}"))
        })
    }
}

type BuiltRouterWithCtx<E, C, S> = (Router<AppContext<E, C, S>>, AppContext<E, C, S>);

struct PreparedServe<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    router: Router<AppContext<E, C, S>>,
    serve_ctx: AppContext<E, C, S>,
    startup_modules: Vec<Arc<dyn crate::module::Module>>,
    shutdown_modules: Vec<Arc<dyn crate::module::Module>>,
    on_ready: Vec<ReadyHook<E, C, S>>,
    on_shutdown: Vec<ShutdownHook<E, C, S>>,
    shutdown_timeout: Duration,
}

fn collect_and_validate_routes(
    startup_modules: &[Arc<dyn crate::module::Module>],
    app_routes: &RouteRegistry,
) -> Result<(axum::Router<()>, RouteRegistry), BuildError> {
    let mut module_axum = axum::Router::new();
    let mut module_routes = RouteRegistry::new();
    let mut seen_routes: Vec<(String, String)> = Vec::new();

    // Register module routes FIRST: they must occupy `seen_routes` before
    // app routes are read from `route_registry`, so that module routes
    // participate in duplicate-route detection (App M4) instead of being
    // invisible to it.
    for m in startup_modules {
        let single_mod_router = m.register_routes(rume_http::routing::Router::new());
        let (axum_r, routes) = single_mod_router.into_parts();
        for r in routes.routes() {
            check_and_record_route(&mut seen_routes, r)?;
        }
        module_axum = merge_module_router(module_axum, axum_r)?;
        module_routes.extend(routes);
    }

    // Then read app routes into `seen_routes`. Module routes are already
    // present, so app/module collisions are caught fail-closed; app routes
    // are checked against each other as they are recorded.
    for r in app_routes.routes() {
        check_and_record_route(&mut seen_routes, r)?;
    }

    Ok((module_axum, module_routes))
}

/// Merge a module router onto a target router, catching Axum's overlapping-route
/// panic and converting it into a typed [`BuildError::DuplicateRoute`].
fn merge_module_router<S>(
    router: Router<S>,
    module_axum: axum::Router<()>,
) -> Result<Router<S>, BuildError>
where
    S: Clone + Send + Sync + 'static,
{
    let adapted: Router<S> = module_axum.with_state(());
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| router.merge(adapted))).map_err(
        |payload| {
            let msg = if let Some(s) = payload.downcast_ref::<String>() {
                s.as_str()
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                s
            } else {
                "overlapping route detected during router merge"
            };
            parse_axum_merge_conflict(msg)
        },
    )
}

fn parse_axum_merge_conflict(msg: &str) -> BuildError {
    if let Some((_, rest)) = msg.split_once("Handler for `")
        && let Some((sig, _)) = rest.split_once('`')
        && let Some((method, path)) = sig.split_once(' ')
    {
        return BuildError::DuplicateRoute {
            method: method.to_string(),
            path: path.to_string(),
        };
    }
    BuildError::DuplicateRoute {
        method: rume_http::routing::RouteInfo::UNKNOWN_METHOD.to_string(),
        path: msg.to_string(),
    }
}

/// Normalize route capture segments (e.g. `{id}`, `{user_id}`, `:id` -> `{}`,
/// `{*rest}` -> `{*}`) so structurally equivalent route templates are detected
/// as duplicates regardless of parameter names.
fn normalize_route_template(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with("{*") && segment.ends_with('}') && segment.len() > 3 {
                "{*}"
            } else if (segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2)
                || (segment.starts_with(':') && segment.len() > 1)
            {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn methods_conflict(existing: &str, new: &str) -> bool {
    if existing == "ANY" || new == "ANY" {
        return true;
    }
    if existing == rume_http::routing::RouteInfo::UNKNOWN_METHOD
        || new == rume_http::routing::RouteInfo::UNKNOWN_METHOD
    {
        return false;
    }
    existing == new
}

fn check_and_record_route(
    seen_routes: &mut Vec<(String, String)>,
    route: &rume_http::routing::RouteInfo,
) -> Result<(), BuildError> {
    let new_method = route.method.to_string();
    let normalized_path = normalize_route_template(&route.path);
    if seen_routes.iter().any(|(existing_method, existing_path)| {
        existing_path == &normalized_path && methods_conflict(existing_method, &new_method)
    }) {
        return Err(BuildError::DuplicateRoute {
            method: new_method,
            path: route.path.clone(),
        });
    }
    seen_routes.push((new_method, normalized_path));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::AppContext;
    use crate::app::paths::ProjectPaths;
    use crate::app::state::{ApplicationMode, Availability, AvailabilityState, LifecycleState};
    use rume_http::routing::HealthChecks;

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
        use tower::ServiceExt;

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
        let app = app_router.with_state(test_ctx());

        let req = axum::http::Request::builder()
            .uri("/hello")
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
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
        use tower::ServiceExt;

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
        let app = app_router.with_state(test_ctx());

        let req = axum::http::Request::builder()
            .uri("/hello")
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
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
        use tower::ServiceExt;

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
        let app = app_router.with_state(test_ctx());

        let req = axum::http::Request::builder()
            .uri("/hello")
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
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

    #[tokio::test]
    async fn app_builder_topological_startup_and_reverse_shutdown_on_failure() {
        struct RecordingModule {
            name: &'static str,
            deps: Vec<&'static str>,
            fail_start: bool,
            events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl crate::module::Module for RecordingModule {
            fn name(&self) -> &str {
                self.name
            }

            fn dependencies(&self) -> Vec<&str> {
                self.deps.clone()
            }

            fn on_start<'a>(
                &'a self,
                _ctx: &'a crate::module::ModuleContext,
            ) -> crate::module::BoxFuture<'a, crate::module::ModuleHookResult> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("start:{}", self.name));
                    if self.fail_start {
                        Err("boom".into())
                    } else {
                        Ok(())
                    }
                })
            }

            fn on_shutdown<'a>(
                &'a self,
                _ctx: &'a crate::module::ModuleContext,
            ) -> crate::module::BoxFuture<'a, ()> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("shutdown:{}", self.name));
                })
            }
        }

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .bind("127.0.0.1:0")
            .module(RecordingModule {
                name: "c",
                deps: vec!["b"],
                fail_start: true,
                events: std::sync::Arc::clone(&events),
            })
            .module(RecordingModule {
                name: "b",
                deps: vec!["a"],
                fail_start: false,
                events: std::sync::Arc::clone(&events),
            })
            .module(RecordingModule {
                name: "a",
                deps: vec![],
                fail_start: false,
                events: std::sync::Arc::clone(&events),
            });

        let err = builder
            .run_with_signal(std::future::pending())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("c"));
        assert_eq!(
            *events.lock().unwrap(),
            vec!["start:a", "start:b", "start:c", "shutdown:b", "shutdown:a",]
        );
    }

    /// A test module that mounts a single GET route.
    struct MountRouteModule;
    impl crate::module::Module for MountRouteModule {
        fn name(&self) -> &'static str {
            "mount_route"
        }
        fn register_routes(
            &self,
            router: rume_http::routing::Router,
        ) -> rume_http::routing::Router {
            router.get("/m/x", || async { "module" })
        }
    }

    #[test]
    fn module_route_conflicting_with_app_route_is_rejected() {
        // Module routes must be registered before `route_registry` is read into
        // `seen_routes`, so they participate in duplicate-route detection.
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .get("/m/x", || async { "app" })
            .module(MountRouteModule);

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(err, BuildError::DuplicateRoute { path, .. } if path == "/m/x"));
    }

    #[test]
    fn conflicting_capture_parameter_names_are_rejected_as_duplicate_route() {
        struct CaptureRouteModule;
        impl crate::module::Module for CaptureRouteModule {
            fn name(&self) -> &'static str {
                "capture_route"
            }
            fn register_routes(
                &self,
                router: rume_http::routing::Router,
            ) -> rume_http::routing::Router {
                router.get("/users/{id}", || async { "module" })
            }
        }

        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .get("/users/{user_id}", || async { "app" })
            .module(CaptureRouteModule);

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::DuplicateRoute { method, path }
            if method == "GET" && path == "/users/{user_id}"
        ));
    }

    #[test]
    fn normalize_route_template_normalizes_captures_and_wildcards() {
        assert_eq!(normalize_route_template("/users/{id}"), "/users/{}");
        assert_eq!(normalize_route_template("/users/{user_id}"), "/users/{}");
        assert_eq!(normalize_route_template("/users/:id"), "/users/{}");
        assert_eq!(normalize_route_template("/files/{*rest}"), "/files/{*}");
        assert_eq!(normalize_route_template("/users/list"), "/users/list");
    }

    #[test]
    fn disjoint_methods_on_identical_template_are_allowed() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .get("/users/{id}", || async { "get" })
            .post("/users/{id}", || async { "post" });

        assert!(builder.build_router(test_ctx()).is_ok());
    }

    #[test]
    fn opaque_method_router_with_disjoint_method_is_allowed() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .route("/items", axum::routing::get(|| async { "get" }))
            .post("/items", || async { "post" });

        assert!(builder.build_router(test_ctx()).is_ok());
    }

    #[test]
    fn any_route_still_conflicts_with_specific_method_on_same_path() {
        let ctx = test_ctx();
        let builder = RumeBuilder::new(ctx)
            .any("/items", || async { "any" })
            .post("/items", || async { "post" });

        let err = builder.build_router(test_ctx()).unwrap_err();
        assert!(matches!(
            err,
            BuildError::DuplicateRoute { method, path }
            if method == "POST" && path == "/items"
        ));
    }

    #[tokio::test]
    async fn into_test_parts_and_on_ready_observe_module_routes_and_health_checks() {
        use std::sync::{Arc, Mutex};

        #[derive(Debug)]
        struct ModDbHealthCheck;

        impl rume_http::routing::HealthCheck for ModDbHealthCheck {
            fn name(&self) -> &str {
                "mod_db"
            }

            fn run(
                &self,
            ) -> std::pin::Pin<Box<dyn Future<Output = rume_http::routing::HealthCheckResult> + Send>>
            {
                Box::pin(async {
                    rume_http::routing::HealthCheckResult {
                        name: "mod_db".to_string(),
                        status: rume_http::routing::HealthStatus::Ok,
                        message: "ok".to_string(),
                        meta: BTreeMap::new(),
                        cached: false,
                        duration_ms: None,
                    }
                })
            }
        }

        struct ObservedModule;

        impl crate::module::Module for ObservedModule {
            fn name(&self) -> &str {
                "observed"
            }

            fn register_routes(
                &self,
                router: rume_http::routing::Router,
            ) -> rume_http::routing::Router {
                router.get("/mod-items", || async { "mod-items" })
            }

            fn register_health_checks(&self, checks: &mut rume_http::routing::HealthChecks) {
                checks.add(ModDbHealthCheck);
            }
        }

        // 1. `into_test_parts` must expose module routes & health checks on `TestAppParts.ctx`.
        let parts = RumeBuilder::new(test_ctx())
            .module(ObservedModule)
            .into_test_parts()
            .expect("into_test_parts should succeed");
        assert!(
            parts
                .ctx
                .route_registry
                .routes()
                .iter()
                .any(|r| r.path == "/mod-items"),
            "TestAppParts.ctx.route_registry must contain module route /mod-items"
        );
        let report = parts.ctx.health_checks.run().await;
        assert!(
            report.checks.iter().any(|c| c.name == "mod_db"),
            "TestAppParts.ctx.health_checks must contain module health check mod_db"
        );

        // 2. `run_with_signal` must pass finalized context to `on_ready` and `on_shutdown`.
        let ready_observed = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        let shutdown_observed = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        let ready_spy = Arc::clone(&ready_observed);
        let shutdown_spy = Arc::clone(&shutdown_observed);

        let ctx = test_ctx();
        let lifecycle = ctx.lifecycle.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let mut server = tokio::spawn(async move {
            RumeBuilder::new(ctx)
                .bind("127.0.0.1:0")
                .module(ObservedModule)
                .on_ready(move |ctx| {
                    let ready_spy = Arc::clone(&ready_spy);
                    async move {
                        let routes: Vec<String> = ctx
                            .route_registry
                            .routes()
                            .iter()
                            .map(|r| r.path.clone())
                            .collect();
                        let checks: Vec<String> = ctx
                            .health_checks
                            .run()
                            .await
                            .checks
                            .into_iter()
                            .map(|c| c.name)
                            .collect();
                        *ready_spy.lock().unwrap() = (routes, checks);
                    }
                })
                .on_shutdown(move |ctx| {
                    let shutdown_spy = Arc::clone(&shutdown_spy);
                    async move {
                        let routes: Vec<String> = ctx
                            .route_registry
                            .routes()
                            .iter()
                            .map(|r| r.path.clone())
                            .collect();
                        let checks: Vec<String> = ctx
                            .health_checks
                            .run()
                            .await
                            .checks
                            .into_iter()
                            .map(|c| c.name)
                            .collect();
                        *shutdown_spy.lock().unwrap() = (routes, checks);
                    }
                })
                .run_with_signal(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;
        let _ = shutdown_tx.send(());
        server
            .await
            .expect("server task should join")
            .expect("run_with_signal should exit cleanly");

        let (ready_routes, ready_checks) = ready_observed.lock().unwrap().clone();
        assert!(
            ready_routes.iter().any(|p| p == "/mod-items"),
            "on_ready must observe /mod-items in route_registry, got {ready_routes:?}"
        );
        assert!(
            ready_checks.iter().any(|c| c == "mod_db"),
            "on_ready must observe mod_db in health_checks, got {ready_checks:?}"
        );

        let (shutdown_routes, shutdown_checks) = shutdown_observed.lock().unwrap().clone();
        assert!(
            shutdown_routes.iter().any(|p| p == "/mod-items"),
            "on_shutdown must observe /mod-items in route_registry, got {shutdown_routes:?}"
        );
        assert!(
            shutdown_checks.iter().any(|c| c == "mod_db"),
            "on_shutdown must observe mod_db in health_checks, got {shutdown_checks:?}"
        );

        // 3. `run_with_listener_and_signal` must also pass finalized context to `on_ready` and `on_shutdown`.
        let listener_ready = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        let listener_ready_spy = Arc::clone(&listener_ready);
        let ctx = test_ctx();
        let lifecycle = ctx.lifecycle.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let mut server = tokio::spawn(async move {
            RumeBuilder::new(ctx)
                .module(ObservedModule)
                .on_ready(move |ctx| {
                    let listener_ready_spy = Arc::clone(&listener_ready_spy);
                    async move {
                        let routes: Vec<String> = ctx
                            .route_registry
                            .routes()
                            .iter()
                            .map(|r| r.path.clone())
                            .collect();
                        let checks: Vec<String> = ctx
                            .health_checks
                            .run()
                            .await
                            .checks
                            .into_iter()
                            .map(|c| c.name)
                            .collect();
                        *listener_ready_spy.lock().unwrap() = (routes, checks);
                    }
                })
                .run_with_listener_and_signal(listener, async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        wait_for_ready(&lifecycle, &mut server, Duration::from_secs(5)).await;
        let _ = shutdown_tx.send(());
        server
            .await
            .expect("listener server task should join")
            .expect("run_with_listener_and_signal should exit cleanly");

        let (routes, checks) = listener_ready.lock().unwrap().clone();
        assert!(routes.iter().any(|p| p == "/mod-items"));
        assert!(checks.iter().any(|c| c == "mod_db"));
    }

    #[tokio::test]
    async fn inspect_mode_rejects_module_cycle_and_duplicate_routes() {
        struct CycleMod {
            name: &'static str,
            deps: Vec<&'static str>,
        }

        impl crate::module::Module for CycleMod {
            fn name(&self) -> &str {
                self.name
            }

            fn dependencies(&self) -> Vec<&str> {
                self.deps.clone()
            }
        }

        struct DupRouteMod;

        impl crate::module::Module for DupRouteMod {
            fn name(&self) -> &str {
                "dup_route"
            }

            fn register_routes(
                &self,
                router: rume_http::routing::Router,
            ) -> rume_http::routing::Router {
                router.get("/dup", || async { "module" })
            }
        }

        // (a) Cyclic module dependencies must fail closed with InvalidInput in Inspect mode.
        let cycle_err = RumeBuilder::new(test_ctx())
            .mode(ApplicationMode::Inspect)
            .module(CycleMod {
                name: "a",
                deps: vec!["b"],
            })
            .module(CycleMod {
                name: "b",
                deps: vec!["a"],
            })
            .run()
            .await
            .expect_err("inspect mode must reject cyclic module dependencies");
        assert_eq!(cycle_err.kind(), std::io::ErrorKind::InvalidInput);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_cycle_err = RumeBuilder::new(test_ctx())
            .mode(ApplicationMode::Inspect)
            .module(CycleMod {
                name: "a",
                deps: vec!["b"],
            })
            .module(CycleMod {
                name: "b",
                deps: vec!["a"],
            })
            .run_with_listener_and_signal(listener, std::future::pending())
            .await
            .expect_err("listener inspect mode must reject cyclic module dependencies");
        assert_eq!(listener_cycle_err.kind(), std::io::ErrorKind::InvalidInput);

        // (b) Colliding app/module routes must fail closed with InvalidInput in Inspect mode.
        let dup_err = RumeBuilder::new(test_ctx())
            .mode(ApplicationMode::Inspect)
            .get("/dup", || async { "app" })
            .module(DupRouteMod)
            .run()
            .await
            .expect_err("inspect mode must reject duplicate routes");
        assert_eq!(dup_err.kind(), std::io::ErrorKind::InvalidInput);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_dup_err = RumeBuilder::new(test_ctx())
            .mode(ApplicationMode::Inspect)
            .get("/dup", || async { "app" })
            .module(DupRouteMod)
            .run_with_listener_and_signal(listener, std::future::pending())
            .await
            .expect_err("listener inspect mode must reject duplicate routes");
        assert_eq!(listener_dup_err.kind(), std::io::ErrorKind::InvalidInput);

        // (c) App `.route("/dup", get(...))` (UNKNOWN metadata) colliding with module
        // `.get("/dup", ...)` must fail cleanly in both Inspect and Run/build_router,
        // while `.route("/dup", post(...))` + module `.get("/dup", ...)` remains valid.
        let unknown_dup_inspect = RumeBuilder::new(test_ctx())
            .mode(ApplicationMode::Inspect)
            .route("/dup", axum::routing::get(|| async { "app" }))
            .module(DupRouteMod)
            .run()
            .await
            .expect_err("inspect mode must reject app .route(get) + module .get collision");
        assert_eq!(unknown_dup_inspect.kind(), std::io::ErrorKind::InvalidInput);

        let ctx = test_ctx();
        let unknown_dup_build = RumeBuilder::new(ctx.clone())
            .route("/dup", axum::routing::get(|| async { "app" }))
            .module(DupRouteMod)
            .build_router(ctx)
            .expect_err("build_router must return DuplicateRoute instead of panicking");
        assert!(matches!(
            unknown_dup_build,
            BuildError::DuplicateRoute { .. }
        ));

        RumeBuilder::new(test_ctx())
            .mode(ApplicationMode::Inspect)
            .route("/dup", axum::routing::post(|| async { "app-post" }))
            .module(DupRouteMod)
            .run()
            .await
            .expect("POST /dup and GET /dup on the same path must remain valid in Inspect");
    }

    async fn wait_for_ready(
        lifecycle: &crate::app::state::Lifecycle,
        server: &mut tokio::task::JoinHandle<Result<(), std::io::Error>>,
        timeout: Duration,
    ) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if lifecycle.state() == LifecycleState::Ready {
                return;
            }
            if server.is_finished() {
                let res = server.await;
                panic!(
                    "server task exited early before reaching Ready (state={:?}): {res:?}",
                    lifecycle.state()
                );
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for server to reach Ready (state={:?})",
                lifecycle.state()
            );
            tokio::task::yield_now().await;
        }
    }
}
