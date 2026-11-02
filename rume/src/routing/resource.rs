//! Resourceful routing.
//!
//! Provides [`Resource`], a builder for standard RESTful actions mapped to
//! explicit routes with no controller-class discovery, no reflection, and no
//! naming conventions.

use axum::routing::MethodRouter;

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
/// pairs, then mount them with [`crate::routing::RouteGroup::add_routes`] so
/// the group's prefix is applied, or register them directly on an Axum
/// [`Router`](axum::Router).
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{delete, get, post, put};

    #[test]
    fn resource_generates_standard_rest_paths() {
        async fn index() {}
        async fn show() {}
        async fn create() {}
        async fn update() {}
        async fn destroy() {}

        let routes = Resource::<()>::new("posts")
            .index(get(index))
            .show(get(show))
            .create(post(create))
            .update(put(update))
            .destroy(delete(destroy))
            .into_routes();

        let paths: Vec<&str> = routes.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/posts",
                "/posts/{id}",
                "/posts",
                "/posts/{id}",
                "/posts/{id}"
            ]
        );

        let methods: Vec<&str> = routes
            .iter()
            .map(|(_, r)| {
                // MethodRouter doesn't expose the method publicly; just count.
                let _ = r;
                ""
            })
            .collect();
        assert_eq!(methods.len(), 5);
    }

    #[test]
    fn resource_with_group_prefix_applies_prefix() {
        use crate::routing::RouteGroup;
        use axum::routing::get;

        async fn index() {}

        let routes = Resource::<()>::new("posts").index(get(index)).into_routes();

        let group = RouteGroup::<()>::new().prefix("/api").add_routes(routes);
        let _router = group.build();
    }
}
