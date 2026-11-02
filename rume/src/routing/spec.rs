//! Route specification.
//!
//! Provides [`RouteSpec`], a builder for route metadata (path, method,
//! parameter matchers, and name) that builds into data suitable for mounting
//! on an Axum [`Router`](axum::routing::Router).
//!
//! Route parameter matchers declared with [`where_param`](Self::where_param)
//! are enforced at HTTP time by applying the returned
//! [`ParamValidationLayer`](crate::routing::spec::ParamValidationLayer) to the
//! route's [`MethodRouter`](axum::routing::MethodRouter). This makes route
//! declarations coherent: a declared `Matcher::Number` actually rejects
//! non-numeric requests with a 400.

use axum::http::Method;
use axum::{
    body::Body,
    extract::{FromRequestParts, Path},
    http::Request,
    response::{IntoResponse, Response},
};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::AppError;
use crate::routing::matchers::Matcher;

/// A route specification that builds into route metadata.
///
/// # Example
///
/// ```ignore
/// use rume::routing::{RouteSpec, Matcher};
/// use rume::prelude::*;
///
/// let (path, method, _, _) = RouteSpec::get("/users/{id}")
///     .where_param("id", Matcher::Number)
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
    /// explicit: the caller constructs a [`crate::routing::RouteInfo`] from
    /// the returned metadata and calls
    /// [`crate::routing::RouteRegistry::register`] on their
    /// application-scoped [`crate::routing::RouteRegistry`] (available via
    /// [`crate::app::AppContext::route_registry`]) when the route is mounted.
    /// Building a route without mounting it therefore never pollutes any
    /// registry.
    pub fn build(self) -> (String, Method, Vec<(String, Matcher)>, Option<String>) {
        (self.path, self.method, self.params, self.name)
    }

    /// Create a validation layer that enforces this route spec's parameter
    /// matchers at HTTP time.
    ///
    /// Apply the layer to the route's [`MethodRouter`](axum::routing::MethodRouter)
    /// before mounting it on a [`Router`](axum::Router). When a request
    /// arrives, path parameters are extracted and validated against each
    /// declared matcher. A missing or invalid parameter produces a 400
    /// [`AppError::BadRequest`] (Problem Details) without running the handler.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rume::routing::{RouteSpec, Matcher};
    /// use axum::Router;
    ///
    /// let spec = RouteSpec::get("/users/{id}")
    ///     .where_param("id", Matcher::Number)
    ///     .as("users.show");
    ///
    /// let (path, _, params, _) = spec.build();
    ///
    /// let router = Router::new()
    ///     .route(
    ///         &path,
    ///         axum::routing::get(get_user).layer(spec.validation_layer()),
    ///     );
    /// ```
    pub fn validation_layer(&self) -> ParamValidationLayer {
        ParamValidationLayer::new(self.params.clone())
    }
}

/// A tower layer that validates route path parameters against declared
/// [`Matcher`]s at HTTP time.
///
/// Created via [`RouteSpec::validation_layer`]. Apply it to a
/// [`MethodRouter`](axum::routing::MethodRouter) so that requests with
/// missing or invalid path parameters receive a 400 response before the
/// handler runs.
#[derive(Clone)]
pub struct ParamValidationLayer {
    matchers: std::sync::Arc<Vec<(String, Matcher)>>,
}

impl ParamValidationLayer {
    pub(crate) fn new(matchers: Vec<(String, Matcher)>) -> Self {
        Self {
            matchers: std::sync::Arc::new(matchers),
        }
    }
}

impl<S> tower::Layer<S> for ParamValidationLayer {
    type Service = ParamValidationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ParamValidationService {
            inner,
            matchers: std::sync::Arc::clone(&self.matchers),
        }
    }
}

/// Service that validates path parameters against matchers before calling the
/// inner service.
#[derive(Clone)]
pub struct ParamValidationService<S> {
    inner: S,
    matchers: std::sync::Arc<Vec<(String, Matcher)>>,
}

impl<S> tower::Service<Request<Body>> for ParamValidationService<S>
where
    S: tower::Service<Request<Body>, Response = Response> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let matchers = std::sync::Arc::clone(&self.matchers);
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let (mut parts, body) = req.into_parts();

            // Axum's route matcher populates extensions with path params before
            // the middleware/service stack runs. Extract them and validate
            // against each declared matcher.
            //
            // S-r2: If parameter extraction fails (e.g. malformed percent-encoding
            // or invalid UTF-8), the layer must fail closed and return 400 Problem
            // Details, rather than silently letting the request pass unvalidated.
            if !matchers.is_empty() {
                match Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &()).await {
                    Ok(Path(params)) => {
                        for (name, matcher) in &*matchers {
                            let value = match params.get(name) {
                                Some(v) => v,
                                None => {
                                    return Ok(AppError::BadRequest {
                                        code: "MISSING_PARAM",
                                        message: format!("missing parameter: {name}"),
                                    }
                                    .into_response());
                                }
                            };
                            if let Err(msg) = matcher.validate(value) {
                                return Ok(AppError::BadRequest {
                                    code: "INVALID_PARAM",
                                    message: msg,
                                }
                                .into_response());
                            }
                        }
                    }
                    Err(_) => {
                        return Ok(AppError::BadRequest {
                            code: "INVALID_PARAM",
                            message: "invalid path parameters".to_string(),
                        }
                        .into_response());
                    }
                }
            }

            let req = Request::from_parts(parts, body);
            inner.call(req).await
        })
    }
}
