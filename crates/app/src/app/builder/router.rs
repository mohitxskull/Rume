use axum::routing::{MethodRouter, Router};

use super::middleware::validate_middleware_config;
use super::{BuildError, RumeBuilder};
use crate::app::context::AppContext;
use rume_http::routing::RouteRegistry;
use std::sync::Arc;

impl<E, C, S> RumeBuilder<E, C, S>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
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
            let full_path = rume_http::routing::registry::join_path(path, &r.path);
            let mut params = rume_http::routing::registry::extract_path_params(&full_path);
            for p in &r.params {
                if !params.contains(p) {
                    params.push(p.clone());
                }
            }
            self.record_route(rume_http::routing::RouteInfo {
                method: r.method.clone(),
                path: full_path,
                name: r.name.clone(),
                params,
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
    pub(crate) fn build_router_with_resolved(
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
        validate_middleware_config(
            &middleware,
            &middleware_order,
            &groups,
            &groups_order,
            &applied_middleware,
            &applied_groups,
        )?;

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
}

type BuiltRouterWithCtx<E, C, S> = (Router<AppContext<E, C, S>>, AppContext<E, C, S>);

pub(crate) fn collect_and_validate_routes(
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
pub(crate) fn merge_module_router<S>(
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
pub(crate) fn normalize_route_template(path: &str) -> String {
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
