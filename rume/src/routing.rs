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

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{FromRequestParts, Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{MethodRouter, Router};

use crate::app::AppContext;

// ---------------------------------------------------------------------------
// Route parameter matchers
// ---------------------------------------------------------------------------

/// A route parameter matcher.
///
/// Used with [`RouteSpec::where`] to validate and cast route parameters.
#[derive(Debug, Clone)]
pub enum Matcher {
    /// Matches digits only, returns the string as-is.
    Number,
    /// Matches UUID format (8-4-4-4-12 hex chars), returns lowercase `String`.
    Uuid,
    /// Matches URL-friendly slugs, returns the string as-is.
    Slug,
}

impl Matcher {
    /// Validate a string value against this matcher.
    pub fn validate(&self, value: &str) -> Result<String, String> {
        match self {
            Matcher::Number => {
                if value.parse::<u64>().is_ok() {
                    Ok(value.to_string())
                } else {
                    Err(format!("{value} is not a valid number"))
                }
            }
            Matcher::Uuid => {
                if is_valid_uuid(value) {
                    Ok(value.to_lowercase())
                } else {
                    Err(format!("{value} is not a valid UUID"))
                }
            }
            Matcher::Slug => {
                if is_valid_slug(value) {
                    Ok(value.to_string())
                } else {
                    Err(format!("{value} is not a valid slug"))
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
    pub struct Number;
    /// Matcher for UUID parameters.
    pub struct Uuid;
    /// Matcher for slug parameters.
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

/// Trait for param matchers used with `ValidatedParam`.
pub trait ParamMatcher {
    const PARAM_NAME: &'static str;
    fn validate(value: &str) -> Result<String, String>;
}

impl<M> FromRequestParts<()> for ValidatedParam<M>
where
    M: ParamMatcher,
{
    type Rejection = Rejection;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &(),
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

/// Global registry of all routes (for `rume list:routes`).
static ROUTE_REGISTRY: OnceLock<Mutex<Vec<RouteInfo>>> = OnceLock::new();

fn route_registry() -> &'static Mutex<Vec<RouteInfo>> {
    ROUTE_REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a route in the global registry.
fn register_route(info: RouteInfo) {
    if let Ok(mut registry) = route_registry().lock() {
        registry.push(info);
    }
}

/// Get all registered routes (for `rume list:routes`).
pub fn registered_routes() -> Vec<RouteInfo> {
    route_registry()
        .lock()
        .map(|r| r.clone())
        .unwrap_or_default()
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

    /// Register the route and build route metadata.
    ///
    /// Returns `(path, method, params, name)` suitable for `Router::route()`.
    pub fn build(self) -> (String, Method, Vec<(String, Matcher)>, Option<String>) {
        let info = RouteInfo {
            method: self.method.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
            params: self.params.iter().map(|(n, _)| n.clone()).collect(),
        };
        register_route(info);

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
    pub fn add(mut self, path: &str, method_router: MethodRouter<S>) -> Self {
        let full_path = if self.prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}{}", self.prefix, path)
        };
        self.inner = self.inner.route(&full_path, method_router);
        self
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

/// Registry of health checks.
#[derive(Debug)]
pub struct HealthChecks {
    checks: Vec<Arc<dyn HealthCheck>>,
}

impl HealthChecks {
    /// Create a new empty health checks registry.
    pub fn new() -> Self {
        Self { checks: Vec::new() }
    }

    /// Register a health check.
    pub fn register(mut self, check: impl HealthCheck + 'static) -> Self {
        self.checks.push(Arc::new(check));
        self
    }

    /// Run all registered checks and produce a report.
    pub async fn run(&self) -> HealthReport {
        let mut checks = Vec::new();
        let mut is_healthy = true;
        let mut status = "ok".to_string();

        for check in &self.checks {
            let result = check.run().await;
            let healthy = matches!(result.status, HealthStatus::Ok);
            if !healthy {
                is_healthy = false;
            }
            if matches!(result.status, HealthStatus::Failed) {
                status = "error".to_string();
            } else if matches!(result.status, HealthStatus::Warning) && status == "ok" {
                status = "warning".to_string();
            }
            checks.push(result);
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
        Self {
            checks: self.checks.clone(),
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
    let mut info = BTreeMap::new();
    info.insert(
        "pid".to_string(),
        serde_json::Value::Number(std::process::id().into()),
    );
    info.insert(
        "platform".to_string(),
        serde_json::Value::String(std::env::consts::OS.to_string()),
    );
    info
}

/// Routes for health checks: `/health/live` and `/health/ready`.
///
/// - `GET /health/live` — liveness probe: returns 200 if the process is alive.
///   Does NOT check dependencies (a DB outage shouldn't trigger a restart).
/// - `GET /health/ready` — readiness probe: runs all registered health checks.
///   Returns 200 when healthy, 503 when any check fails.
///
/// Both routes use [`State<AppContext<E, C>>`] to access availability and
/// health checks from the application context.
pub fn health_routes<E, C>() -> Router<AppContext<E, C>>
where
    E: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/health/live",
            axum::routing::get(move |State(ctx): State<AppContext<E, C>>| async move {
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
            axum::routing::get(move |State(ctx): State<AppContext<E, C>>| async move {
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
