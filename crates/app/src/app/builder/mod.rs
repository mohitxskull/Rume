//! Rume application builder.
//!
//! `Rume` is a marker type for the builder API. Every Rume application carries
//! an [`AppContext`] as its Axum state.
//!
//! Submodules: `middleware` (named middleware and groups), `router` (route
//! mounting and router construction).

mod middleware;
mod router;

use axum::routing::Router;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

use crate::app::builder::middleware::validate_middleware_config;
use crate::app::builder::router::{collect_and_validate_routes, merge_module_router};
use crate::app::context::AppContext;
use crate::app::paths::ProjectPaths;
use crate::app::state::{ApplicationMode, AvailabilityState, LifecycleState};
use rume_http::routing::RouteRegistry;

pub use self::middleware::MiddlewareConfigError;

/// Errors that can occur during router construction or application preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// Middleware configuration error.
    Middleware(MiddlewareConfigError),
    /// CORS configuration error.
    Cors(rume_http::transport::CorsConfigError),
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
            Self::Cors(err) => write!(f, "{err}"),
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

impl From<rume_http::transport::CorsConfigError> for BuildError {
    fn from(err: rume_http::transport::CorsConfigError) -> Self {
        Self::Cors(err)
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
        if self.0.contains(':') && !(self.0.starts_with('[') && self.0.ends_with(']')) {
            format!("[{}]:{}", self.0, self.1)
        } else {
            format!("{}:{}", self.0, self.1)
        }
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
    /// Transport-level maximum request body size in bytes (default: 2 MiB; `None` disables the cap).
    body_limit: Option<usize>,
    /// Optional fail-closed CORS configuration (opt-in; `None` by default).
    cors: Option<rume_http::transport::CorsConfig>,
    /// Optional HTTP response compression configuration (opt-in; `None` by default).
    compression: Option<rume_http::transport::CompressionConfig>,
    /// Optional HTTP header & JSON PII redaction configuration for `ObservabilityLayer`.
    redaction: Option<rume_observability::RedactionConfig>,
    /// Optional native TLS configuration (feature-gated under `tls`).
    #[cfg(feature = "tls")]
    tls: Option<rume_http::tls::TlsConfig>,
    /// Optional real-time WebSocket/SSE hub.
    realtime: Option<rume_http::realtime::RealtimeHub>,
}

impl<E, C, S> RumeBuilder<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
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
            body_limit: Some(rume_http::transport::DEFAULT_BODY_LIMIT_BYTES),
            cors: None,
            compression: None,
            redaction: None,
            #[cfg(feature = "tls")]
            tls: None,
            realtime: None,
        }
    }

    fn record_route(&self, info: rume_http::routing::RouteInfo) {
        self.route_registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register(info);
    }

    pub fn module(mut self, module: impl crate::module::Module) -> Self {
        self.modules.push(Arc::new(module));
        self
    }

    pub fn register_module(self, module: impl crate::module::Module) -> Self {
        self.module(module)
    }

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

    pub fn bind(mut self, addr: impl IntoBindAddr) -> Self {
        self.bind = addr.into_bind_string();
        self
    }

    pub fn mode(mut self, mode: ApplicationMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn state(mut self, state: S) -> Self {
        self.ctx.state = state;
        self
    }

    pub fn paths(mut self, paths: ProjectPaths) -> Self {
        self.ctx.paths = paths;
        self
    }

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

    pub fn on_shutdown<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(AppContext<E, C, S>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_shutdown.push(Box::new(move |ctx| Box::pin(f(ctx))));
        self
    }

    pub fn lifecycle(&self) -> crate::app::state::Lifecycle {
        self.ctx.lifecycle.clone()
    }

    pub fn availability(&self) -> crate::app::state::Availability {
        self.ctx.availability.clone()
    }

    pub fn paths_ref(&self) -> &ProjectPaths {
        &self.ctx.paths
    }

    pub fn health_checks(&self) -> rume_http::routing::HealthChecks {
        self.ctx.health_checks.clone()
    }

    pub fn shutdown_timeout(mut self, duration: Duration) -> Self {
        self.shutdown_timeout = duration;
        self
    }

    /// Set the transport-level maximum request body size in bytes (default: 2 MiB).
    pub fn body_limit(mut self, max_bytes: usize) -> Self {
        self.body_limit = Some(max_bytes);
        self
    }

    /// Disable the transport-level request body size limit.
    pub fn without_body_limit(mut self) -> Self {
        self.body_limit = None;
        self
    }

    /// Enable fail-closed CORS middleware with the given [`rume_http::transport::CorsConfig`].
    pub fn cors(mut self, config: rume_http::transport::CorsConfig) -> Self {
        self.cors = Some(config);
        self
    }

    /// Enable HTTP response compression (gzip, brotli, zstd) with the given [`rume_http::transport::CompressionConfig`].
    pub fn compression(mut self, config: rume_http::transport::CompressionConfig) -> Self {
        self.compression = Some(config);
        self
    }

    /// Configure structured PII redaction on the framework [`ObservabilityLayer`](crate::app::observability::ObservabilityLayer).
    pub fn redaction(mut self, config: rume_observability::RedactionConfig) -> Self {
        self.redaction = Some(config);
        self
    }

    /// Enable native TLS (HTTP/1.1 + HTTP/2 ALPN, and optional ACME) with the given [`rume_http::tls::TlsConfig`].
    #[cfg(feature = "tls")]
    pub fn tls(mut self, config: rume_http::tls::TlsConfig) -> Self {
        self.tls = Some(config);
        self
    }

    /// Attach a [`rume_http::realtime::RealtimeHub`] to application extensions and graceful shutdown.
    pub fn realtime(mut self, hub: rume_http::realtime::RealtimeHub) -> Self {
        self.realtime = Some(hub);
        self
    }

    /// Serve static files from disk (with optional SPA fallback) as the router fallback.
    pub fn static_files(mut self, config: rume_http::static_files::StaticFilesConfig) -> Self {
        self.router = self
            .router
            .fallback_service(rume_http::static_files::static_files_router(config));
        self
    }

    /// Serve compile-time embedded assets via `rust-embed` as the router fallback.
    #[cfg(feature = "static-embed")]
    pub fn embedded_assets<A>(
        mut self,
        config: rume_http::static_files::EmbeddedAssetsConfig,
    ) -> Self
    where
        A: rume_http::static_files::RustEmbed + Send + Sync + 'static,
    {
        self.router =
            self.router
                .fallback_service(rume_http::static_files::embedded_assets_router::<A, _>(
                    config,
                ));
        self
    }

    /// Apply an [`rume_http::http_cache::HttpCacheLayer`] to the application router.
    pub fn http_cache(mut self, layer: rume_http::http_cache::HttpCacheLayer) -> Self {
        self.router = self.router.layer(layer);
        self
    }

    fn effective_mode(&self) -> ApplicationMode {
        if std::env::var_os("RUME_INSPECT").is_some()
            || std::env::args_os().any(|a| {
                a.as_os_str() == std::ffi::OsStr::new("--inspect")
                    || a.as_os_str() == std::ffi::OsStr::new("--rume-inspect-routes")
            })
        {
            ApplicationMode::Inspect
        } else {
            self.mode
        }
    }

    pub(crate) fn run_inspect_mode(mut self) -> Result<(), std::io::Error> {
        // Why: `Inspect` mode runs `ModuleRegistry` DAG validation, middleware
        // validation, CORS config validation, `collect_and_validate_routes`, and
        // the actual Axum router merge so module cycles, misconfigured
        // middleware, invalid CORS configs, and duplicate routes fail closed
        // without starting the server. It intentionally skips `on_start`,
        // `on_ready`, and `on_shutdown` hooks so `rume list:routes` and boot
        // checks remain side-effect-free, and also skips
        // `Module::register_health_checks` (so duplicate health-check names are
        // detected by the serving build rather than `Inspect`).
        let resolved = crate::app::lifecycle::resolve_modules(&self.modules)?;
        validate_middleware_config(
            &self.middleware,
            &self.middleware_order,
            &self.groups,
            &self.groups_order,
            &self.applied_middleware,
            &self.applied_groups,
        )?;
        if let Some(ref cors_cfg) = self.cors {
            let _ = cors_cfg.build_layer().map_err(BuildError::from)?;
        }
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

        if std::env::var_os("RUME_INSPECT").as_deref() == Some(std::ffi::OsStr::new("routes"))
            || std::env::args_os()
                .any(|a| a.as_os_str() == std::ffi::OsStr::new("--rume-inspect-routes"))
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
        #[cfg(feature = "tls")]
        let tls = self.tls.take();
        let realtime = self.realtime.clone();
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
            #[cfg(feature = "tls")]
            tls,
            realtime,
        })
    }

    pub async fn run(self) -> Result<(), std::io::Error> {
        self.run_with_signal(crate::app::shutdown::shutdown_signal())
            .await
    }

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
            #[cfg(feature = "tls")]
            tls,
            realtime,
        } = prepared;

        let addr = listener.local_addr()?;

        // Shutdown signal monitoring for boot hooks and server.
        // Use a watch channel instead of AtomicBool+Notify so the shutdown
        // signal latches: `rx.wait_for()` returns immediately if `send(true)`
        // already happened before the waiter registered, preventing the
        // lost-wakeup hang (RC-01).
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        let sf_task = shutdown_tx.clone();
        let signal_handle = tokio::spawn(async move {
            signal.await;
            let _ = sf_task.send(true);
        });
        // Why: abort the signal watcher task on any early exit (startup failure,
        // bind/serve error, or cancellation) so it never leaks as a detached task.
        struct AbortOnDrop(tokio::task::JoinHandle<()>);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _signal_guard = AbortOnDrop(signal_handle);

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

        // --- Step 7: Serve traffic with immediate lifecycle & availability transition on shutdown ---
        let shutdown_lifecycle = serve_ctx.lifecycle.clone();
        let shutdown_availability = serve_ctx.availability.clone();
        let shutdown_realtime = realtime.clone();
        let drain_check_rx = shutdown_rx.clone();

        let graceful_signal = async move {
            let _ = shutdown_rx.wait_for(|&shutting_down| shutting_down).await;
            tracing::info!(
                "shutdown signal received: transitioning to Terminating / RefusingTraffic"
            );
            shutdown_lifecycle.set(LifecycleState::Terminating);
            shutdown_availability.set(AvailabilityState::RefusingTraffic);
            // Why: initiate_shutdown only broadcasts the shutdown signal so the HTTP/TLS
            // accept loop exits immediately instead of blocking on connection drain.
            if let Some(hub) = shutdown_realtime {
                hub.initiate_shutdown();
            }
        };

        let result = {
            #[cfg(feature = "tls")]
            {
                if let Some(tls_config) = tls {
                    println!("listening on https://{addr}");
                    rume_http::tls::serve_tls_listener(
                        listener,
                        router.with_state(serve_ctx.clone()),
                        &tls_config,
                        graceful_signal,
                        shutdown_timeout,
                    )
                    .await
                } else {
                    println!("listening on http://{addr}");
                    axum::serve(
                        listener,
                        router
                            .with_state(serve_ctx.clone())
                            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    )
                    .with_graceful_shutdown(graceful_signal)
                    .await
                }
            }
            #[cfg(not(feature = "tls"))]
            {
                println!("listening on http://{addr}");
                axum::serve(
                    listener,
                    router
                        .with_state(serve_ctx.clone())
                        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(graceful_signal)
                .await
            }
        };

        if let Some(hub) = realtime {
            if *drain_check_rx.borrow() {
                hub.wait_drained().await;
            } else {
                hub.shutdown().await;
            }
        }

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
    #[cfg(feature = "tls")]
    tls: Option<rume_http::tls::TlsConfig>,
    realtime: Option<rume_http::realtime::RealtimeHub>,
}

#[cfg(test)]
mod tests;
