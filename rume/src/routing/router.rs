//! Canonical application router.
//!
//! Provides [`Router`], which wraps an [`axum::Router`] and maintains an
//! application-scoped [`RouteRegistry`].
//!
//! Mounting a route and recording its metadata are one atomic operation.

use axum::handler::Handler;
use axum::http::Method;
use axum::routing::MethodRouter;
use tower::Layer;

use crate::routing::registry::{RouteInfo, RouteRegistry, extract_path_params, join_path};
use crate::routing::resource::Resource;
use crate::routing::spec::RouteSpec;

/// A canonical application router that carries both the Axum HTTP routing tree
/// and the inspectable [`RouteRegistry`].
#[derive(Debug, Clone)]
pub struct Router<S = ()> {
    inner: axum::Router<S>,
    registry: RouteRegistry,
}

impl<S> Default for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Create a new, empty router with an empty route registry.
    pub fn new() -> Self {
        Self {
            inner: axum::Router::new(),
            registry: RouteRegistry::new(),
        }
    }

    /// Mount a `GET` route and register its metadata.
    pub fn get<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, axum::routing::get(handler));
        self.registry.register(RouteInfo {
            method: Method::GET,
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount a `POST` route and register its metadata.
    pub fn post<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, axum::routing::post(handler));
        self.registry.register(RouteInfo {
            method: Method::POST,
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount a `PUT` route and register its metadata.
    pub fn put<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, axum::routing::put(handler));
        self.registry.register(RouteInfo {
            method: Method::PUT,
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount a `PATCH` route and register its metadata.
    pub fn patch<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, axum::routing::patch(handler));
        self.registry.register(RouteInfo {
            method: Method::PATCH,
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount a `DELETE` route and register its metadata.
    pub fn delete<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, axum::routing::delete(handler));
        self.registry.register(RouteInfo {
            method: Method::DELETE,
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount an `ANY` route and register its metadata.
    pub fn any<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, axum::routing::any(handler));
        self.registry.register(RouteInfo {
            method: Method::from_bytes(b"ANY").unwrap_or(Method::GET),
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
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
        self.inner = self.inner.route_service(path, service);
        self.registry.register(RouteInfo {
            method: Method::from_bytes(b"ANY").unwrap_or(Method::GET),
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount a fallback handler for unmatched requests.
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.fallback(handler);
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
        self.inner = self.inner.fallback_service(service);
        self
    }

    /// Mount a standard Axum [`MethodRouter`].
    ///
    /// When using raw [`MethodRouter`], method information cannot be inspected
    /// at runtime by Axum, so it is recorded as `ANY` in the registry.
    /// Prefer method-specific helpers (`.get()`, `.post()`, or `.route_spec()`)
    /// for exact metadata reflection.
    pub fn route(mut self, path: &str, method_router: MethodRouter<S>) -> Self {
        self.inner = self.inner.route(path, method_router);
        self.registry.register(RouteInfo {
            method: Method::from_bytes(b"ANY").unwrap_or(Method::GET),
            path: path.to_string(),
            name: None,
            params: extract_path_params(path),
        });
        self
    }

    /// Mount a route defined by a [`RouteSpec`].
    ///
    /// Atomically:
    /// 1. Applies the parameter validation layer ([`RouteSpec::validation_layer`])
    ///    to the handler.
    /// 2. Mounts the route on the underlying Axum router.
    /// 3. Registers the route's exact method, path, name, and parameters in the
    ///    route registry.
    pub fn route_spec<H, T>(mut self, spec: RouteSpec, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let method = spec.method().clone();
        let path = spec.path().to_string();
        let name = spec.name().map(String::from);
        let params = spec.param_names();
        let validation_layer = spec.validation_layer();

        let method_router = match method {
            Method::GET => axum::routing::get(handler),
            Method::POST => axum::routing::post(handler),
            Method::PUT => axum::routing::put(handler),
            Method::PATCH => axum::routing::patch(handler),
            Method::DELETE => axum::routing::delete(handler),
            _ => axum::routing::any(handler),
        };

        let method_router = method_router.layer(validation_layer);
        self.inner = self.inner.route(&path, method_router);
        self.registry.register(RouteInfo {
            method,
            path,
            name,
            params,
        });
        self
    }

    /// Mount a RESTful [`Resource`].
    ///
    /// Atomically mounts all resource actions and registers their paths,
    /// methods, and resource-qualified names (`{resource}.index`, `{resource}.show`, etc.).
    pub fn resource(mut self, resource: Resource<S>) -> Self {
        for r in resource.routes() {
            self.inner = self.inner.route(&r.path, r.router.clone());
            self.registry.register(RouteInfo {
                method: r.method.clone(),
                path: r.path.clone(),
                name: Some(r.name.clone()),
                params: r.params.clone(),
            });
        }
        self
    }

    /// Nest a child [`Router`] under a path prefix.
    ///
    /// All routes from the child router are registered in the parent registry
    /// with their full prefixed paths.
    pub fn nest(mut self, path: &str, other: Router<S>) -> Self {
        self.inner = self.inner.nest(path, other.inner);
        for r in other.registry.routes() {
            self.registry.register(RouteInfo {
                method: r.method.clone(),
                path: join_path(path, &r.path),
                name: r.name.clone(),
                params: r.params.clone(),
            });
        }
        self
    }

    /// Merge another [`Router`] into this one.
    ///
    /// Merges both the Axum routing tree and the route registries.
    pub fn merge(mut self, other: Router<S>) -> Self {
        self.inner = self.inner.merge(other.inner);
        self.registry.extend(other.registry);
        self
    }

    /// Merge a plain [`axum::Router`] into this router.
    ///
    /// Uninstrumented third-party Axum routers do not expose route metadata,
    /// so the route registry remains unchanged.
    pub fn merge_axum(mut self, axum_router: axum::Router<S>) -> Self {
        self.inner = self.inner.merge(axum_router);
        self
    }

    /// Apply a Tower layer to the router.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower::Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Error:
            Into<std::convert::Infallible> + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.inner = self.inner.layer(layer);
        self
    }

    /// Provide the state for this router, transitioning `S` to a new state type.
    pub fn with_state<S2>(self, state: S) -> Router<S2> {
        Router {
            inner: self.inner.with_state(state),
            registry: self.registry,
        }
    }

    /// Consume this router and return the underlying [`axum::Router`].
    pub fn into_inner(self) -> axum::Router<S> {
        self.inner
    }

    /// Consume this router and return both the [`axum::Router`] and [`RouteRegistry`].
    pub fn into_parts(self) -> (axum::Router<S>, RouteRegistry) {
        (self.inner, self.registry)
    }

    /// Access the route registry.
    pub fn registry(&self) -> &RouteRegistry {
        &self.registry
    }

    /// Access the route registry mutably.
    pub fn registry_mut(&mut self) -> &mut RouteRegistry {
        &mut self.registry
    }

    /// Return all registered routes.
    pub fn routes(&self) -> &[RouteInfo] {
        self.registry.routes()
    }

    /// Merge a stateless [`Router<()>`] into this router.
    ///
    /// The stateless router's routes and metadata are merged into this router.
    /// This enables domain modules that encapsulate their own state via `.with_state(my_state)`
    /// (or stateless modules) to be merged cleanly into an application router carrying any state type `S`.
    pub fn merge_stateless(mut self, stateless: Router<()>) -> Self {
        let (axum_router, registry) = stateless.into_parts();
        let adapted: axum::Router<S> = axum_router.with_state(());
        self.inner = self.inner.merge(adapted);
        self.registry.extend(registry);
        self
    }
}

impl<S> From<Router<S>> for axum::Router<S> {
    fn from(router: Router<S>) -> Self {
        router.inner
    }
}

impl<B> tower::Service<axum::http::Request<B>> for Router<()>
where
    axum::Router<()>: tower::Service<axum::http::Request<B>>,
{
    type Response = <axum::Router<()> as tower::Service<axum::http::Request<B>>>::Response;
    type Error = <axum::Router<()> as tower::Service<axum::http::Request<B>>>::Error;
    type Future = <axum::Router<()> as tower::Service<axum::http::Request<B>>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        tower::Service::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, req: axum::http::Request<B>) -> Self::Future {
        tower::Service::call(&mut self.inner, req)
    }
}

impl<S> From<axum::Router<S>> for Router<S> {
    fn from(axum_router: axum::Router<S>) -> Self {
        Self {
            inner: axum_router,
            registry: RouteRegistry::new(),
        }
    }
}
