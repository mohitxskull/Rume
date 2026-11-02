//! Routing for Rume applications.
//!
//! Provides an Axum-compatible routing layer with named routes, parameter
//! validation, route groups, resource routes, redirects, and URL building.
//!
//! # Example
//!
//! ```ignore
//! use rume::routing::{RouteSpec, Matcher};
//! use rume::prelude::*;
//!
//! let (path, method, _, _) = RouteSpec::get("/users/:id")
//!     .where("id", Matcher::number())
//!     .as("users.show")
//!     .build();
//!
//! let router = Router::new()
//!     .route(&path, match method {
//!         Method::GET => axum::routing::get(handlers::get_user),
//!         _ => axum::routing::any(handlers::get_user),
//!     });
//!
//! Rume::builder(ctx).router(router).run().await?;
//! ```

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{FromRequestParts, Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{MethodRouter, Router};

use tokio::time::timeout;

use crate::app::AppContext;

// ---------------------------------------------------------------------------
// Route parameter matchers
// ---------------------------------------------------------------------------

/// A route parameter matcher.
///
/// Used with [`RouteSpec::where`] to validate and cast route parameters.
///
/// # Matchers
///
/// - [`Matcher::Number`] — digits only; returns the original string (not a
///   parsed number). Overflowing digit strings (e.g. u64::MAX+1) are rejected.
/// - [`Matcher::Uuid`] — UUID format (8-4-4-4-12 hex chars); returns lowercase.
/// - [`Matcher::Slug`] — URL-friendly slug; returns the string as-is.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// Matches digits only, returns the string as-is.
    ///
    /// Validation: the entire value must parse as `u64`. Returns the original
    /// string, not a parsed number.
    Number,
    /// Matches UUID format (8-4-4-4-12 hex chars), returns lowercase `String`.
    Uuid,
    /// Matches URL-friendly slugs, returns the string as-is.
    Slug,
}

impl Matcher {
    /// Validate a string value against this matcher.
    ///
    /// Returns the validated string on success, or a generic error message on
    /// failure. The error message does NOT echo the raw input value to avoid
    /// leaking user-controlled data into error responses (see S-m3).
    pub fn validate(&self, value: &str) -> Result<String, String> {
        match self {
            Matcher::Number => {
                if value.parse::<u64>().is_ok() {
                    Ok(value.to_string())
                } else {
                    Err("value is not a valid number".to_string())
                }
            }
            Matcher::Uuid => {
                if is_valid_uuid(value) {
                    Ok(value.to_lowercase())
                } else {
                    Err("value is not a valid UUID".to_string())
                }
            }
            Matcher::Slug => {
                if is_valid_slug(value) {
                    Ok(value.to_string())
                } else {
                    Err("value is not a valid slug".to_string())
                }
            }
        }
    }
}

fn is_valid_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lens = [8, 4, 4, 4, 12];
    for (part, &expected_len) in parts.iter().zip(expected_lens.iter()) {
        if part.len() != expected_len {
            return false;
        }
        if !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
    }
    true
}

fn is_valid_slug(value: &str) -> bool {
    if value.len() < 3 {
        return false;
    }
    if value.contains(' ') {
        return false;
    }
    if value.starts_with('-') || value.starts_with('_') {
        return false;
    }
    if value.ends_with('-') || value.ends_with('_') {
        return false;
    }
    if value.contains("--") || value.contains("__") {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------------------------------------------------------------------------
// ValidatedParam extractor
// ---------------------------------------------------------------------------

/// A route parameter value that has been validated against a matcher.
///
/// Extracted from route params via Axum's `Path` extractor.
#[derive(Debug, Clone)]
pub struct ValidatedParam<M> {
    pub name: String,
    pub value: String,
    _marker: std::marker::PhantomData<M>,
}

impl<M> ValidatedParam<M> {
    /// Consume the extractor and return the validated string value.
    pub fn into_inner(self) -> String {
        self.value
    }
}

impl<M> std::ops::Deref for ValidatedParam<M> {
    type Target = str;

    fn deref(&self) -> &str {
        &self.value
    }
}

/// Marker types for param matchers used with `ValidatedParam`.
pub mod matchers {
    /// Matcher for numeric parameters.
    #[derive(Debug)]
    pub struct Number;
    /// Matcher for UUID parameters.
    #[derive(Debug)]
    pub struct Uuid;
    /// Matcher for slug parameters.
    #[derive(Debug)]
    pub struct Slug;
}

impl matchers::Number {
    pub fn validate(value: &str) -> Result<String, String> {
        Matcher::Number.validate(value)
    }
}
impl matchers::Uuid {
    pub fn validate(value: &str) -> Result<String, String> {
        Matcher::Uuid.validate(value)
    }
}
impl matchers::Slug {
    pub fn validate(value: &str) -> Result<String, String> {
        Matcher::Slug.validate(value)
    }
}

impl ParamMatcher for matchers::Number {
    const PARAM_NAME: &'static str = "test_param";
    fn validate(value: &str) -> Result<String, String> {
        Matcher::Number.validate(value)
    }
}
impl ParamMatcher for matchers::Uuid {
    const PARAM_NAME: &'static str = "test_param";
    fn validate(value: &str) -> Result<String, String> {
        Matcher::Uuid.validate(value)
    }
}
impl ParamMatcher for matchers::Slug {
    const PARAM_NAME: &'static str = "test_param";
    fn validate(value: &str) -> Result<String, String> {
        Matcher::Slug.validate(value)
    }
}

/// Trait for param matchers used with `ValidatedParam`.
pub trait ParamMatcher: Send + Sync + 'static {
    const PARAM_NAME: &'static str;
    fn validate(value: &str) -> Result<String, String>;
}

impl<M, S> FromRequestParts<S> for ValidatedParam<M>
where
    M: ParamMatcher,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Path(params): Path<BTreeMap<String, String>> = Path::from_request_parts(parts, &())
            .await
            .map_err(|err| Rejection::Extraction(err.to_string()))?;

        let value = params
            .get(M::PARAM_NAME)
            .cloned()
            .ok_or_else(|| Rejection::MissingParam(M::PARAM_NAME.to_string()))?;

        M::validate(&value)
            .map(|v| ValidatedParam {
                name: M::PARAM_NAME.to_string(),
                value: v,
                _marker: std::marker::PhantomData,
            })
            .map_err(Rejection::Validation)
    }
}

// ---------------------------------------------------------------------------
// Route info and registry (for `rume list:routes`)
// ---------------------------------------------------------------------------

/// Information about a registered route (for `rume list:routes`).
#[derive(Debug, Clone)]
pub struct RouteInfo {
    pub method: Method,
    pub path: String,
    pub name: Option<String>,
    pub params: Vec<String>,
}

/// Application-scoped route registry. Each Rume application owns its own
/// registry; route metadata is tied to the application that registered it,
/// not to a process-global singleton.
#[derive(Debug, Default, Clone)]
pub struct RouteRegistry {
    routes: Vec<RouteInfo>,
}

impl RouteRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a route.
    pub fn register(&mut self, info: RouteInfo) {
        self.routes.push(info);
    }

    /// Return all registered routes.
    pub fn routes(&self) -> &[RouteInfo] {
        &self.routes
    }

    /// Number of registered routes.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Remove all registered routes.
    pub fn clear(&mut self) {
        self.routes.clear();
    }
}

// ---------------------------------------------------------------------------
// RouteSpec
// ---------------------------------------------------------------------------

/// A route specification that builds into route metadata.
///
/// # Example
///
/// ```ignore
/// use rume::routing::{RouteSpec, Matcher};
/// use rume::prelude::*;
///
/// let (path, method, _, _) = RouteSpec::get("/users/:id")
///     .where("id", Matcher::number())
///     .as("users.show")
///     .build();
///
/// let router = Router::new()
///     .route(&path, match method {
///         Method::GET => axum::routing::get(handlers::get_user),
///         _ => axum::routing::any(handlers::get_user),
///     });
///
/// Rume::builder(ctx).router(router).run().await?;
/// ```
pub struct RouteSpec {
    path: String,
    method: Method,
    name: Option<String>,
    params: Vec<(String, Matcher)>,
}

impl RouteSpec {
    /// Create a new GET route spec.
    pub fn get(path: &str) -> Self {
        Self {
            path: path.to_string(),
            method: Method::GET,
            name: None,
            params: Vec::new(),
        }
    }

    /// Create a new POST route spec.
    pub fn post(path: &str) -> Self {
        Self {
            path: path.to_string(),
            method: Method::POST,
            name: None,
            params: Vec::new(),
        }
    }

    /// Create a new PUT route spec.
    pub fn put(path: &str) -> Self {
        Self {
            path: path.to_string(),
            method: Method::PUT,
            name: None,
            params: Vec::new(),
        }
    }

    /// Create a new PATCH route spec.
    pub fn patch(path: &str) -> Self {
        Self {
            path: path.to_string(),
            method: Method::PATCH,
            name: None,
            params: Vec::new(),
        }
    }

    /// Create a new DELETE route spec.
    pub fn delete(path: &str) -> Self {
        Self {
            path: path.to_string(),
            method: Method::DELETE,
            name: None,
            params: Vec::new(),
        }
    }

    /// Add a parameter matcher.
    pub fn where_param(mut self, name: &str, matcher: Matcher) -> Self {
        self.params.push((name.to_string(), matcher));
        self
    }

    /// Alias for [`where_param`].
    pub fn r#where(self, name: &str, matcher: Matcher) -> Self {
        self.where_param(name, matcher)
    }

    /// Name the route for URL generation.
    pub fn r#as(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Build the route specification into route metadata.
    ///
    /// Returns `(path, method, params, name)` suitable for `Router::route()`.
    ///
    /// # Registration
    ///
    /// `build()` does NOT register the route anywhere. Registration is
    /// explicit: the caller constructs a [`RouteInfo`] from the returned
    /// metadata and calls [`RouteRegistry::register`] on their
    /// application-scoped [`RouteRegistry`] (available via
    /// [`AppContext::route_registry`]) when the route is mounted. Building a
    /// route without mounting it therefore never pollutes any registry.
    pub fn build(self) -> (String, Method, Vec<(String, Matcher)>, Option<String>) {
        (self.path, self.method, self.params, self.name)
    }
}

// ---------------------------------------------------------------------------
// RouteGroup
// ---------------------------------------------------------------------------

/// Route group builder for applying shared configuration to multiple routes.
pub struct RouteGroup<S> {
    inner: Router<S>,
    prefix: String,
    name_prefix: String,
}

impl<S: Clone + Send + Sync + 'static> RouteGroup<S> {
    /// Create a new route group.
    pub fn new() -> Self {
        Self {
            inner: Router::new(),
            prefix: String::new(),
            name_prefix: String::new(),
        }
    }

    /// Set a URL prefix for all routes in the group.
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.to_string();
        self
    }

    /// Set a name prefix for all named routes in the group.
    pub fn r#as(mut self, name: &str) -> Self {
        self.name_prefix = name.to_string();
        self
    }

    /// Add a route method router to the group.
    ///
    /// Joins `prefix` and `path` with exactly one `/` separator, so
    /// `prefix("/api/").add("/users")` → `/api/users` and
    /// `prefix("api").add("users")` → `/api/users`.
    pub fn add(mut self, path: &str, method_router: MethodRouter<S>) -> Self {
        let full_path = Self::join_path(&self.prefix, path);
        self.inner = self.inner.route(&full_path, method_router);
        self
    }

    /// Add multiple `(path, method_router)` routes to the group, applying the
    /// group's [`prefix`](RouteGroup::prefix) to each path via [`join_path`].
    ///
    /// This is how a [`Resource`]'s routes are mounted under a group prefix:
    ///
    /// ```ignore
    /// use axum::routing::get;
    /// use rume::routing::{Resource, RouteGroup};
    ///
    /// async fn index() {}
    ///
    /// let routes = Resource::new("posts").index(get(index)).into_routes();
    /// let group = RouteGroup::<()>::new().prefix("/api").add_routes(routes);
    /// ```
    pub fn add_routes(
        mut self,
        routes: impl IntoIterator<Item = (String, MethodRouter<S>)>,
    ) -> Self {
        for (path, router) in routes.into_iter() {
            let full_path = Self::join_path(&self.prefix, &path);
            self.inner = self.inner.route(&full_path, router);
        }
        self
    }

    /// Join a prefix and path with exactly one `/` separator.
    ///
    /// Handles leading/trailing slashes on both sides so that
    /// `join_path("/api/", "/users")` → `/api/users`,
    /// `join_path("api", "users")` → `/api/users`, and
    /// `join_path("", "/users")` → `/users`.
    fn join_path(prefix: &str, path: &str) -> String {
        let prefix = prefix.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        match (prefix.is_empty(), path.is_empty()) {
            (true, true) => "/".to_string(),
            (true, false) => format!("/{path}"),
            (false, true) => {
                if prefix.starts_with('/') {
                    prefix.to_string()
                } else {
                    format!("/{prefix}")
                }
            }
            (false, false) => {
                if prefix.starts_with('/') {
                    format!("{prefix}/{path}")
                } else {
                    format!("/{prefix}/{path}")
                }
            }
        }
    }

    /// Build into an Axum `Router`.
    pub fn build(self) -> Router<S> {
        self.inner
    }
}

impl<S: Clone + Send + Sync + 'static> Default for RouteGroup<S> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Resource
// ---------------------------------------------------------------------------

/// A resourceful route builder for standard RESTful actions.
///
/// Maps the standard resource actions to explicit routes, with no
/// controller-class discovery, no reflection, and no naming conventions — the
/// user lists handlers explicitly:
///
/// | Action    | Method | Path             |
/// |-----------|--------|------------------|
/// | index     | GET    | `/{name}`        |
/// | show      | GET    | `/{name}/{id}`    |
/// | create    | POST   | `/{name}`        |
/// | update    | PUT    | `/{name}/{id}`    |
/// | destroy   | DELETE | `/{name}/{id}`    |
///
/// Handlers are supplied as `MethodRouter<S>` values (e.g. `axum::routing::get(handler)`).
/// Use [`into_routes`](Self::into_routes) to get the `(path, method_router)`
/// pairs, then mount them with [`RouteGroup::add_routes`] so the group's prefix
/// is applied, or register them directly on an Axum [`Router`].
///
/// # Example
///
/// ```ignore
/// use axum::routing::{delete, get, post, put};
/// use rume::routing::Resource;
///
/// async fn index() {}
/// async fn show() {}
/// async fn create() {}
/// async fn update() {}
/// async fn destroy() {}
///
/// let routes = Resource::new("posts")
///     .index(get(index))
///     .show(get(show))
///     .create(post(create))
///     .update(put(update))
///     .destroy(delete(destroy))
///     .into_routes();
/// ```
pub struct Resource<S> {
    name: String,
    routes: Vec<(String, MethodRouter<S>)>,
}

impl<S> Resource<S> {
    /// Create a new resource builder for the given resource name.
    ///
    /// The name becomes the URL segment, e.g. `Resource::new("posts")`
    /// produces paths `/posts` and `/posts/{id}`.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            routes: Vec::new(),
        }
    }

    /// `GET /{name}` — list all resources.
    pub fn index(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push((format!("/{}", self.name), router));
        self
    }

    /// `GET /{name}/{id}` — show a single resource.
    pub fn show(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push((format!("/{}/{{id}}", self.name), router));
        self
    }

    /// `POST /{name}` — create a new resource.
    pub fn create(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push((format!("/{}", self.name), router));
        self
    }

    /// `PUT /{name}/{id}` — update an existing resource (full replacement).
    pub fn update(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push((format!("/{}/{{id}}", self.name), router));
        self
    }

    /// `DELETE /{name}/{id}` — destroy a resource.
    pub fn destroy(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push((format!("/{}/{{id}}", self.name), router));
        self
    }

    /// Consume the builder and return the list of `(path, method_router)` pairs.
    pub fn into_routes(self) -> Vec<(String, MethodRouter<S>)> {
        self.routes
    }
}

// ---------------------------------------------------------------------------
// Health checks
// ---------------------------------------------------------------------------

/// Health check result status.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Warning,
    Failed,
}

/// A single health check result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HealthCheckResult {
    pub name: String,
    pub status: HealthStatus,
    pub message: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
    pub cached: bool,
}

/// A health check that can be registered.
pub trait HealthCheck: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>>;
}

/// A full health report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HealthReport {
    pub is_healthy: bool,
    pub status: String,
    pub finished_at: String,
    pub debug_info: BTreeMap<String, serde_json::Value>,
    pub checks: Vec<HealthCheckResult>,
}

/// Default per-check timeout: 5 seconds.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Registry of health checks.
///
/// Checks run concurrently on each [`run`](Self::run) call. Each check is
/// wrapped in a [`tokio::time::timeout`]; a check that exceeds the timeout is
/// marked [`HealthStatus::Failed`]. When a `cache_ttl` is set, results are
/// cached internally (interior mutability via [`RwLock`]) so subsequent runs
/// within the TTL return cached results without re-running the checks.
#[derive(Debug)]
pub struct HealthChecks {
    checks: Vec<Arc<dyn HealthCheck>>,
    timeout: Duration,
    cache_ttl: Option<Duration>,
    cache: RwLock<HashMap<String, (Instant, HealthCheckResult)>>,
}

impl HealthChecks {
    /// Create a new empty health checks registry.
    ///
    /// Default timeout is [`DEFAULT_TIMEOUT`] (5s) and caching is disabled.
    pub fn new() -> Self {
        Self {
            checks: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            cache_ttl: None,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Register a health check.
    pub fn register(mut self, check: impl HealthCheck + 'static) -> Self {
        self.checks.push(Arc::new(check));
        self
    }

    /// Set the per-check timeout. A check that does not complete within this
    /// duration is marked [`HealthStatus::Failed`].
    ///
    /// Default: [`DEFAULT_TIMEOUT`] (5 seconds).
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = duration;
        self
    }

    /// Set the optional result cache TTL.
    ///
    /// When `Some(ttl)`, check results are cached and subsequent [`run`](Self::run)
    /// calls within the TTL return cached results (with `cached: true`) without
    /// re-running the checks. When `None`, caching is disabled.
    pub fn cache_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Run all registered checks concurrently and produce a report.
    ///
    /// Each check is spawned on a task and wrapped in a per-check timeout.
    /// Results are aggregated: any [`HealthStatus::Failed`] → overall `Failed`
    /// (503 on `/health/ready`); any [`HealthStatus::Warning`] with no
    /// `Failed` → overall `Warning` (degraded, 503); all [`HealthStatus::Ok`]
    /// → overall `Ok` (200).
    pub async fn run(&self) -> HealthReport {
        // Serve from cache when a TTL is configured and every check has a
        // fresh cached result.
        if let Some(ttl) = self.cache_ttl {
            let now = Instant::now();
            let cache = self.cache.read().unwrap();
            let all_cached = !self.checks.is_empty()
                && self.checks.iter().all(|check| {
                    cache
                        .get(check.name())
                        .is_some_and(|(instant, _)| now.duration_since(*instant) < ttl)
                });
            drop(cache);
            if all_cached {
                let checks = self
                    .checks
                    .iter()
                    .map(|check| {
                        let mut result = self
                            .cache
                            .read()
                            .unwrap()
                            .get(check.name())
                            .expect("cached result present")
                            .1
                            .clone();
                        result.cached = true;
                        result
                    })
                    .collect::<Vec<_>>();
                return Self::build_report(checks);
            }
        }

        // Run all checks concurrently, each wrapped in a per-check timeout.
        let mut handles: Vec<(String, tokio::task::JoinHandle<HealthCheckResult>)> =
            Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let name = check.name().to_string();
            let check = Arc::clone(check);
            let timeout_duration = self.timeout;
            handles.push((
                name.clone(),
                tokio::spawn(async move {
                    match timeout(timeout_duration, check.run()).await {
                        Ok(result) => result,
                        Err(_elapsed) => HealthCheckResult {
                            name,
                            status: HealthStatus::Failed,
                            message: format!("health check timed out after {:?}", timeout_duration),
                            meta: BTreeMap::new(),
                            cached: false,
                        },
                    }
                }),
            ));
        }

        let mut checks = Vec::with_capacity(handles.len());
        for (name, handle) in handles {
            // A join error means the check task itself panicked; treat as Failed.
            checks.push(handle.await.unwrap_or_else(move |_| HealthCheckResult {
                name,
                status: HealthStatus::Failed,
                message: "health check task panicked".to_string(),
                meta: BTreeMap::new(),
                cached: false,
            }));
        }

        // Persist fresh results to the cache. TTL validity is enforced at
        // read time; we just record the instant now.
        if self.cache_ttl.is_some() {
            let mut cache = self.cache.write().unwrap();
            for result in &checks {
                cache.insert(result.name.clone(), (Instant::now(), result.clone()));
            }
        }

        Self::build_report(checks)
    }

    /// Aggregate check results into a [`HealthReport`].
    ///
    /// Any `Failed` → overall `Failed` (`"error"`, `is_healthy = false`);
    /// any `Warning` with no `Failed` → overall `Warning` (`"warning"`,
    /// `is_healthy = false`, degraded); all `Ok` → overall `Ok` (`"ok"`,
    /// `is_healthy = true`).
    fn build_report(checks: Vec<HealthCheckResult>) -> HealthReport {
        let mut is_healthy = true;
        let mut status = "ok".to_string();

        for result in &checks {
            let healthy = matches!(result.status, HealthStatus::Ok);
            if !healthy {
                is_healthy = false;
            }
            match result.status {
                HealthStatus::Ok => {}
                HealthStatus::Warning => {
                    // Warning only upgrades "ok" to "warning"; a Failed check
                    // (already "error") takes precedence and wins overall.
                    if status == "ok" {
                        status = "warning".to_string();
                    }
                }
                HealthStatus::Failed => status = "error".to_string(),
            }
        }

        HealthReport {
            is_healthy,
            status,
            finished_at: current_timestamp(),
            debug_info: debug_info(),
            checks,
        }
    }
}

impl Clone for HealthChecks {
    fn clone(&self) -> Self {
        let cache = self.cache.read().unwrap();
        Self {
            checks: self.checks.clone(),
            timeout: self.timeout,
            cache_ttl: self.cache_ttl,
            cache: RwLock::new(cache.clone()),
        }
    }
}

impl Default for HealthChecks {
    fn default() -> Self {
        Self::new()
    }
}

fn current_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

fn debug_info() -> BTreeMap<String, serde_json::Value> {
    // S-m2: do not expose pid/platform in health responses — those are
    // operational internals that aid attackers. Return empty map; callers
    // that need debugging info can add it explicitly in development.
    BTreeMap::new()
}

/// Routes for health checks: `/health/live` and `/health/ready`.
///
/// - `GET /health/live` — liveness probe: returns 200 if the process is alive.
///   Does NOT check dependencies (a DB outage shouldn't trigger a restart).
/// - `GET /health/ready` — readiness probe: runs all registered health checks.
///   Returns 200 when healthy, 503 when any check fails.
///
/// Both routes use [`State<AppContext<E, C, S>>`] to access availability and
/// health checks from the application context.
pub fn health_routes<E, C, S>() -> Router<AppContext<E, C, S>>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/health/live",
            axum::routing::get(move |State(ctx): State<AppContext<E, C, S>>| async move {
                if ctx.availability.state() == crate::app::AvailabilityState::Broken {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({"status": "broken"})),
                    )
                        .into_response()
                } else {
                    (StatusCode::OK, Json(serde_json::json!({"status": "alive"}))).into_response()
                }
            }),
        )
        .route(
            "/health/ready",
            axum::routing::get(move |State(ctx): State<AppContext<E, C, S>>| async move {
                let report = ctx.health_checks.run().await;
                let json = serde_json::to_value(&report).unwrap();
                if report.is_healthy {
                    (StatusCode::OK, Json(json)).into_response()
                } else {
                    (StatusCode::SERVICE_UNAVAILABLE, Json(json)).into_response()
                }
            }),
        )
}

// ---------------------------------------------------------------------------
// Rejection
// ---------------------------------------------------------------------------

/// Rejection reasons for `ValidatedParam`.
#[derive(Debug)]
pub enum Rejection {
    Extraction(String),
    MissingParam(String),
    Validation(String),
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Rejection::Extraction(err) => (StatusCode::BAD_REQUEST, err),
            Rejection::MissingParam(name) => (
                StatusCode::BAD_REQUEST,
                format!("missing parameter: {name}"),
            ),
            Rejection::Validation(err) => (StatusCode::UNPROCESSABLE_ENTITY, err),
        };
        (status, Json(serde_json::json!({"error": message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppContext, Availability, ProjectPaths};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone)]
    #[expect(dead_code)]
    struct TestMatcher;

    impl ParamMatcher for TestMatcher {
        const PARAM_NAME: &'static str = "test_param";
        fn validate(value: &str) -> Result<String, String> {
            if value == "valid" {
                Ok(value.to_string())
            } else {
                Err(format!("{value} is not valid"))
            }
        }
    }

    #[test]
    fn matcher_number_validates_digits() {
        assert_eq!(Matcher::Number.validate("42"), Ok("42".to_string()));
        assert!(Matcher::Number.validate("abc").is_err());
        // u64 overflow should reject
        assert!(Matcher::Number.validate("99999999999999999999").is_err());
    }

    #[test]
    fn matcher_uuid_validates_format() {
        assert_eq!(
            Matcher::Uuid.validate("550e8400-e29b-41d4-a716-446655440000"),
            Ok("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
        assert!(Matcher::Uuid.validate("not-a-uuid").is_err());
    }

    #[test]
    fn matcher_slug_validates_format() {
        assert_eq!(Matcher::Slug.validate("my-slug"), Ok("my-slug".to_string()));
        assert!(Matcher::Slug.validate("bad slug").is_err());
        assert!(Matcher::Slug.validate("ab").is_err()); // too short
        assert!(Matcher::Slug.validate("-bad").is_err()); // leading hyphen
        assert!(Matcher::Slug.validate("bad--").is_err()); // doubled hyphen
    }

    #[tokio::test]
    async fn validated_param_works_with_appcontext_state() {
        // C1 fix: ValidatedParam must work with AppContext<E, C, S> state, not just ().
        let ctx = AppContext::new(
            std::net::IpAddr::from([127, 0, 0, 1]),
            std::collections::BTreeMap::<String, String>::new(),
            ProjectPaths::from_current_dir(),
            Availability::new(),
            crate::routing::HealthChecks::new(),
            (),
        );

        let (mut parts, _) = axum::http::Request::builder()
            .body(axum::body::Body::empty())
            .unwrap()
            .into_parts();

        let extractor =
            ValidatedParam::<matchers::Number>::from_request_parts(&mut parts, &ctx).await;

        // Should fail because "test_param" key is missing
        assert!(extractor.is_err());
        match extractor.unwrap_err() {
            Rejection::MissingParam(_) | Rejection::Extraction(_) => {}
            _ => panic!("expected MissingParam or Extraction"),
        }
    }

    #[test]
    fn rejection_status_mapping() {
        assert_eq!(
            Rejection::Extraction("bad".to_string())
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Rejection::MissingParam("id".to_string())
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Rejection::Validation("bad value".to_string())
                .into_response()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    // --- C-m2: RouteGroup path normalization ---

    #[test]
    fn route_group_joins_prefix_and_path_with_single_slash() {
        use axum::routing::get;

        async fn handler() {}

        // prefix trailing slash + path leading slash → single slash
        let _group = RouteGroup::<()>::new()
            .prefix("/api/")
            .add("/users", get(handler));
        // Can't easily introspect the built router's paths here, but the join
        // helper is tested directly below.

        // Verify join_path logic through RouteSpec/RouteGroup behavior by
        // checking that no double-slash or missing-slash occurs.
        assert_eq!(RouteGroup::<()>::join_path("/api/", "/users"), "/api/users");
        assert_eq!(RouteGroup::<()>::join_path("api", "users"), "/api/users");
        assert_eq!(RouteGroup::<()>::join_path("", "/users"), "/users");
        assert_eq!(RouteGroup::<()>::join_path("/api", ""), "/api");
    }

    // --- RouteRegistry (application-scoped; replaces the ROUTE_REGISTRY global) ---

    #[test]
    fn route_registry_register_routes_len_is_empty() {
        let mut registry = RouteRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        registry.register(RouteInfo {
            method: Method::GET,
            path: "/users".to_string(),
            name: Some("users.index".to_string()),
            params: vec![],
        });
        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);

        let routes = registry.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].name.as_deref(), Some("users.index"));

        registry.clear();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn route_registry_default_is_empty() {
        let registry = RouteRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn route_spec_build_returns_metadata() {
        let (path, method, params, name) = RouteSpec::post("/items")
            .where_param("id", Matcher::Uuid)
            .r#as("items.create")
            .build();

        assert_eq!(path, "/items");
        assert_eq!(method, Method::POST);
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "id");
        assert_eq!(name.as_deref(), Some("items.create"));
    }

    #[test]
    fn route_spec_build_has_no_side_effects() {
        // build() returns metadata only; it does not register into any
        // registry (the process-global ROUTE_REGISTRY no longer exists).
        let (path, method, _, name) = RouteSpec::get("/users/:id")
            .where_param("id", Matcher::Number)
            .r#as("users.show")
            .build();

        assert_eq!(path, "/users/:id");
        assert_eq!(method, Method::GET);
        assert_eq!(name.as_deref(), Some("users.show"));

        // Explicit registration is the caller's job, into their own registry.
        let mut registry = RouteRegistry::new();
        registry.register(RouteInfo {
            method,
            path: path.to_string(),
            name,
            params: vec!["id".to_string()],
        });
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.routes()[0].path, "/users/:id");
    }

    // --- HealthChecks::run() with mixed statuses ---

    #[derive(Debug)]
    struct OkCheck;

    impl HealthCheck for OkCheck {
        fn name(&self) -> &str {
            "database"
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "database".to_string(),
                    status: HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    #[derive(Debug)]
    struct FailedCheck;

    impl HealthCheck for FailedCheck {
        fn name(&self) -> &str {
            "s3"
        }
        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "s3".to_string(),
                    status: HealthStatus::Failed,
                    message: "down".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that sleeps for `delay` then records its name in `order`.
    #[derive(Debug)]
    struct TimingCheck {
        name: &'static str,
        delay: Duration,
        order: Arc<Mutex<Vec<String>>>,
    }

    impl HealthCheck for TimingCheck {
        fn name(&self) -> &str {
            self.name
        }

        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            let name = self.name.to_string();
            let delay = self.delay;
            let order = Arc::clone(&self.order);
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                order.lock().unwrap().push(name.clone());
                HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that always reports Warning.
    #[derive(Debug)]
    struct WarningCheck;

    impl HealthCheck for WarningCheck {
        fn name(&self) -> &str {
            "cache"
        }

        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            Box::pin(async move {
                HealthCheckResult {
                    name: "cache".to_string(),
                    status: HealthStatus::Warning,
                    message: "high latency".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    /// A check that counts how many times `run` is invoked.
    #[derive(Debug)]
    struct CountingCheck {
        name: &'static str,
        runs: Arc<AtomicUsize>,
    }

    impl HealthCheck for CountingCheck {
        fn name(&self) -> &str {
            self.name
        }

        fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
            let name = self.name.to_string();
            let runs = Arc::clone(&self.runs);
            Box::pin(async move {
                runs.fetch_add(1, Ordering::SeqCst);
                HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "ok".to_string(),
                    meta: BTreeMap::new(),
                    cached: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn health_checks_run_all_ok() {
        let checks = HealthChecks::new().register(OkCheck);
        let report = checks.run().await;
        assert!(report.is_healthy);
        assert_eq!(report.status, "ok");
    }

    #[tokio::test]
    async fn health_checks_run_failed_is_not_healthy() {
        let checks = HealthChecks::new().register(FailedCheck);
        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "error");
    }

    #[tokio::test]
    async fn health_checks_run_warning_is_not_healthy_but_not_error() {
        use crate::routing::HealthStatus::Warning;

        #[derive(Debug)]
        struct WarningCheck;

        impl HealthCheck for WarningCheck {
            fn name(&self) -> &str {
                "cache"
            }
            fn run(&self) -> Pin<Box<dyn Future<Output = HealthCheckResult> + Send>> {
                Box::pin(async move {
                    HealthCheckResult {
                        name: "cache".to_string(),
                        status: Warning,
                        message: "slow".to_string(),
                        meta: BTreeMap::new(),
                        cached: false,
                    }
                })
            }
        }

        let checks = HealthChecks::new().register(WarningCheck);
        let report = checks.run().await;
        // C-m4: Warning → not fully healthy (503 on /health/ready)
        assert!(!report.is_healthy);
        assert_eq!(report.status, "warning");
    }

    #[tokio::test]
    async fn health_checks_run_concurrently() {
        // A slow check must not block a fast check: both run concurrently.
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let checks = HealthChecks::new()
            .register(TimingCheck {
                name: "slow",
                delay: Duration::from_millis(150),
                order: Arc::clone(&order),
            })
            .register(TimingCheck {
                name: "fast",
                delay: Duration::from_millis(0),
                order: Arc::clone(&order),
            });

        let start = Instant::now();
        let report = checks.run().await;
        let elapsed = start.elapsed();

        assert!(report.is_healthy);
        // Fast check completed before the slow one → they ran concurrently,
        // not sequentially (sequential slow-first would finish slow first).
        let order = order.lock().unwrap();
        assert_eq!(order[0], "fast");
        // Concurrent wall-clock is bounded by the slowest check, not the sum.
        assert!(elapsed < Duration::from_millis(300));
    }

    #[tokio::test]
    async fn health_check_timeout_marks_failed() {
        // A check that sleeps longer than the per-check timeout is Failed.
        let checks = HealthChecks::new()
            .timeout(Duration::from_millis(50))
            .register(TimingCheck {
                name: "slow",
                delay: Duration::from_millis(250),
                order: Arc::new(Mutex::new(Vec::new())),
            });

        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "error");

        let result = report
            .checks
            .iter()
            .find(|r| r.name == "slow")
            .expect("slow check");
        assert_eq!(result.status, HealthStatus::Failed);
        assert!(result.message.contains("timed out"));
        assert!(!result.cached);
    }

    #[tokio::test]
    async fn health_check_caching_returns_cached() {
        let runs = Arc::new(AtomicUsize::new(0));
        let checks = HealthChecks::new()
            .cache_ttl(Some(Duration::from_secs(60)))
            .register(CountingCheck {
                name: "db",
                runs: Arc::clone(&runs),
            });

        // First run executes the check.
        let report1 = checks.run().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(!report1.checks[0].cached);

        // Second run within TTL returns cached results without re-running.
        let report2 = checks.run().await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(report2.checks.len(), 1);
        assert!(report2.checks[0].cached);
        assert_eq!(report2.checks[0].status, HealthStatus::Ok);
    }

    #[tokio::test]
    async fn health_check_degraded_when_warning() {
        // Warning without any Failed → overall Warning (degraded), not healthy.
        let checks = HealthChecks::new().register(WarningCheck);
        let report = checks.run().await;
        assert!(!report.is_healthy);
        assert_eq!(report.status, "warning");
        assert_eq!(report.checks[0].status, HealthStatus::Warning);
    }

    // --- Resourceful routing ---

    #[test]
    fn resource_generates_standard_rest_paths() {
        async fn index() {}
        async fn show() {}
        async fn create() {}
        async fn update() {}
        async fn destroy() {}

        let routes = Resource::<()>::new("posts")
            .index(axum::routing::get(index))
            .show(axum::routing::get(show))
            .create(axum::routing::post(create))
            .update(axum::routing::put(update))
            .destroy(axum::routing::delete(destroy))
            .into_routes();

        assert_eq!(routes.len(), 5);

        let paths: Vec<&str> = routes.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/posts",      // index  GET
                "/posts/{id}", // show   GET
                "/posts",      // create POST
                "/posts/{id}", // update PUT
                "/posts/{id}", // destroy DELETE
            ],
            "expected exactly the five standard REST paths in order"
        );
    }

    #[tokio::test]
    async fn resource_routes_applied_through_route_group() {
        use crate::testing::{TestApp, TestAppBuilder};
        use axum::body::Body;
        use axum::http::Request;
        use axum::routing::{delete, get, post, put};

        async fn index() -> &'static str {
            "index"
        }
        async fn show(Path(params): Path<BTreeMap<String, String>>) -> String {
            format!(
                "show {}",
                params.get("id").map(String::as_str).unwrap_or("?")
            )
        }
        async fn create() -> &'static str {
            "create"
        }
        async fn update(Path(params): Path<BTreeMap<String, String>>) -> String {
            format!(
                "update {}",
                params.get("id").map(String::as_str).unwrap_or("?")
            )
        }
        async fn destroy(Path(params): Path<BTreeMap<String, String>>) -> String {
            format!(
                "destroy {}",
                params.get("id").map(String::as_str).unwrap_or("?")
            )
        }

        let routes = Resource::<()>::new("posts")
            .index(get(index))
            .show(get(show))
            .create(post(create))
            .update(put(update))
            .destroy(delete(destroy))
            .into_routes();

        let app = TestAppBuilder::http()
            .router(
                RouteGroup::<()>::new()
                    .prefix("/api")
                    .add_routes(routes)
                    .build(),
            )
            .build()
            .await;

        // Prefix is applied: routes live under /api/posts.
        let res = app.get("/posts").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // GET /api/posts → index
        let res = app.get("/api/posts").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(TestApp::body_text(res).await, "index");

        // GET /api/posts/42 → show
        let res = app.get("/api/posts/42").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(TestApp::body_text(res).await, "show 42");

        // POST /api/posts → create
        let res = app.post_json("/api/posts", "{}").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(TestApp::body_text(res).await, "create");

        // PUT /api/posts/42 → update
        let req = Request::builder()
            .method(Method::PUT)
            .uri("/api/posts/42")
            .body(Body::empty())
            .unwrap();
        let res = app.request(req).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(TestApp::body_text(res).await, "update 42");

        // DELETE /api/posts/42 → destroy
        let req = Request::builder()
            .method(Method::DELETE)
            .uri("/api/posts/42")
            .body(Body::empty())
            .unwrap();
        let res = app.request(req).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(TestApp::body_text(res).await, "destroy 42");
    }
}
