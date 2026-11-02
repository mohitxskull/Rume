//! Route specification.
//!
//! Provides [`RouteSpec`], a builder for route metadata (path, method,
//! parameter matchers, and name) that builds into data suitable for mounting
//! on an Axum [`Router`](axum::routing::Router).

use axum::http::Method;

use crate::routing::matchers::Matcher;

/// A route specification that builds into route metadata.
///
/// # Example
///
/// ```ignore
/// use rume::routing::{RouteSpec, Matcher};
/// use rume::prelude::*;
///
/// let (path, method, _, _) = RouteSpec::get("/users/:id")
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
}
