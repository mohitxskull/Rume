//! Application bootstrap, lifecycle, and developer-experience helpers.

use axum::Router;
use axum::routing::MethodRouter;
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
pub enum LifecycleState {
    Created,
    Booted,
    Ready,
    Terminating,
    Terminated,
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
    pub fn state(&self) -> LifecycleState {
        match self.inner.load(Ordering::SeqCst) {
            1 => LifecycleState::Booted,
            2 => LifecycleState::Ready,
            3 => LifecycleState::Terminating,
            4 => LifecycleState::Terminated,
            _ => LifecycleState::Created,
        }
    }

    pub(crate) fn set(&self, state: LifecycleState) {
        self.inner.store(state as u8, Ordering::SeqCst);
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
pub enum AvailabilityState {
    Unknown,
    Live,
    Ready,
    RefusingTraffic,
    Broken,
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
    pub fn state(&self) -> AvailabilityState {
        match self.inner.load(Ordering::SeqCst) {
            1 => AvailabilityState::Live,
            2 => AvailabilityState::Ready,
            3 => AvailabilityState::RefusingTraffic,
            4 => AvailabilityState::Broken,
            _ => AvailabilityState::Unknown,
        }
    }

    pub fn set(&self, state: AvailabilityState) {
        self.inner.store(state as u8, Ordering::SeqCst);
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

/// Application context holding env, config, paths, availability, and lifecycle.
///
/// Every Rume application is built with an [`AppContext`]. Handlers access env,
/// config, paths, and availability through [`State<AppContext<E, C>>`].
///
/// # Example
///
/// ```ignore
/// use rume::app::{AppContext, Rume};
/// use rume::prelude::*;
///
/// let ctx = AppContext::new(env, config, paths, availability);
/// let router = routes::router(ctx.clone());
///
/// Rume::builder(ctx)
///     .router(router)
///     .bind((env.host, env.port))
///     .run()
///     .await?;
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
}

impl<E, C> AppContext<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    /// Create a new application context.
    pub fn new(env: E, config: C, paths: ProjectPaths, availability: Availability) -> Self {
        Self {
            env,
            config,
            paths,
            availability,
            lifecycle: Lifecycle::new(),
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
    /// This is the only way to build a Rume application. Every app carries an
    /// [`AppContext`] so handlers can access env, config, paths, and
    /// availability through [`State`].
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
}

impl<E, C> RumeBuilder<E, C>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    pub fn new(ctx: AppContext<E, C>) -> Self {
        Self {
            ctx,
            router: Router::new(),
            bind: "127.0.0.1:3000".to_string(),
            mode: ApplicationMode::Run,
            environment: Environment::from_env(),
            on_ready: Vec::new(),
            on_shutdown: Vec::new(),
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

    /// Run the application until a shutdown signal is received.
    pub async fn run(mut self) -> Result<(), std::io::Error> {
        let addr: SocketAddr = self
            .bind
            .parse()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;

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

        let ctx = self.ctx.clone();
        let result = axum::serve(listener, self.router.with_state(ctx))
            .with_graceful_shutdown(shutdown_signal())
            .await;

        let ctx = self.ctx.clone();
        ctx.lifecycle.set(LifecycleState::Terminating);
        ctx.availability.set(AvailabilityState::RefusingTraffic);

        for hook in self.on_shutdown.drain(..) {
            hook(ctx.clone()).await;
        }

        ctx.lifecycle.set(LifecycleState::Terminated);
        ctx.availability.set(AvailabilityState::Unknown);

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
}

/// Initialize the default tracing subscriber.
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = fmt().with_env_filter(filter).try_init();
}

/// Ops endpoints for infrastructure probes (Kubernetes liveness/readiness).
///
/// Adds `GET /health` and `GET /ready` routes to an existing router, backed
/// by an [`Availability`] handle. The routes capture `availability` directly
/// (not from Axum state), so they work with any state type.
///
/// # Example
///
/// ```ignore
/// use rume::app::{AppContext, ops_router};
/// use rume::prelude::*;
///
/// let ctx = AppContext::new(env, config, paths, availability.clone());
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
            "/health",
            axum::routing::get(move || {
                let availability = health_availability.clone();
                async move {
                    let status = if availability.state() == AvailabilityState::Broken {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        axum::http::StatusCode::OK
                    };
                    let body = OpsStatus {
                        status: if availability.state() == AvailabilityState::Broken {
                            "broken"
                        } else {
                            "ok"
                        },
                    };
                    (status, axum::Json(body))
                }
            }),
        )
        .route(
            "/ready",
            axum::routing::get(move || {
                let availability = ready_availability.clone();
                async move {
                    if availability.is_ready() {
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(OpsStatus { status: "ready" }),
                        )
                    } else {
                        (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(OpsStatus {
                                status: "not ready",
                            }),
                        )
                    }
                }
            }),
        )
}

#[derive(serde::Serialize)]
struct OpsStatus {
    status: &'static str,
}
