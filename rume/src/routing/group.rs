//! Route groups.
//!
//! Provides [`RouteGroup`], a builder for applying shared configuration
//! (prefix, name prefix) to multiple routes before mounting them on an Axum
//! router.

use axum::Router;
use axum::routing::MethodRouter;

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
    /// This is how a [`crate::routing::Resource`]'s routes are mounted under a
    /// group prefix:
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    #[test]
    fn route_group_joins_prefix_and_path_with_single_slash() {
        async fn handler() {}

        let group = RouteGroup::<()>::new()
            .prefix("/api")
            .add("/users", get(handler))
            .add("posts", get(handler));

        let routes = group.build();
        // Just verify it builds without panicking; path normalization is
        // covered by join_path unit tests below.
        let _ = routes;
    }

    #[test]
    fn route_group_join_path_normalizes_slashes() {
        assert_eq!(RouteGroup::<()>::join_path("/api/", "/users"), "/api/users");
        assert_eq!(RouteGroup::<()>::join_path("api", "users"), "/api/users");
        assert_eq!(RouteGroup::<()>::join_path("", "/users"), "/users");
        assert_eq!(RouteGroup::<()>::join_path("/api/", ""), "/api");
        assert_eq!(RouteGroup::<()>::join_path("api", ""), "/api");
        assert_eq!(RouteGroup::<()>::join_path("", ""), "/");
    }
}
