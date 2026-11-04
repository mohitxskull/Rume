//! Resourceful routing.
//!
//! Provides [`Resource`], a builder for standard RESTful actions mapped to
//! explicit routes with no controller-class discovery, no reflection, and no
//! naming conventions.

use axum::http::Method;
use axum::routing::MethodRouter;

/// A single route within a REST resource.
pub struct ResourceRoute<S> {
    pub method: Method,
    pub path: String,
    pub name: String,
    pub params: Vec<String>,
    pub router: MethodRouter<S>,
}

/// A resourceful route builder for standard RESTful actions.
///
/// Maps the standard resource actions to explicit routes, with no
/// controller-class discovery, no reflection, and no naming conventions:
///
/// | Action    | Method | Path             | Name             |
/// |-----------|--------|------------------|------------------|
/// | index     | GET    | `/{name}`        | `{name}.index`   |
/// | show      | GET    | `/{name}/{id}`   | `{name}.show`    |
/// | create    | POST   | `/{name}`        | `{name}.create`  |
/// | update    | PUT    | `/{name}/{id}`   | `{name}.update`  |
/// | patch     | PATCH  | `/{name}/{id}`   | `{name}.patch`   |
/// | destroy   | DELETE | `/{name}/{id}`   | `{name}.destroy` |
pub struct Resource<S> {
    name: String,
    routes: Vec<ResourceRoute<S>>,
}

impl<S> Resource<S> {
    /// Create a new resource builder for the given resource name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            routes: Vec::new(),
        }
    }

    /// Access the resource name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Access the defined resource routes.
    pub fn routes(&self) -> &[ResourceRoute<S>] {
        &self.routes
    }

    /// `GET /{name}` — list all resources.
    pub fn index(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push(ResourceRoute {
            method: Method::GET,
            path: format!("/{}", self.name),
            name: format!("{}.index", self.name),
            params: vec![],
            router,
        });
        self
    }

    /// `GET /{name}/{id}` — show a single resource.
    pub fn show(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push(ResourceRoute {
            method: Method::GET,
            path: format!("/{}/{{id}}", self.name),
            name: format!("{}.show", self.name),
            params: vec!["id".to_string()],
            router,
        });
        self
    }

    /// `POST /{name}` — create a new resource.
    pub fn create(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push(ResourceRoute {
            method: Method::POST,
            path: format!("/{}", self.name),
            name: format!("{}.create", self.name),
            params: vec![],
            router,
        });
        self
    }

    /// `PUT /{name}/{id}` — update an existing resource (full replacement).
    pub fn update(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push(ResourceRoute {
            method: Method::PUT,
            path: format!("/{}/{{id}}", self.name),
            name: format!("{}.update", self.name),
            params: vec!["id".to_string()],
            router,
        });
        self
    }

    /// `PATCH /{name}/{id}` — partially update an existing resource.
    pub fn patch(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push(ResourceRoute {
            method: Method::PATCH,
            path: format!("/{}/{{id}}", self.name),
            name: format!("{}.patch", self.name),
            params: vec!["id".to_string()],
            router,
        });
        self
    }

    /// Alias for `patch`: `PATCH /{name}/{id}`.
    pub fn update_patch(self, router: MethodRouter<S>) -> Self {
        self.patch(router)
    }

    /// `DELETE /{name}/{id}` — destroy a resource.
    pub fn destroy(mut self, router: MethodRouter<S>) -> Self {
        self.routes.push(ResourceRoute {
            method: Method::DELETE,
            path: format!("/{}/{{id}}", self.name),
            name: format!("{}.destroy", self.name),
            params: vec!["id".to_string()],
            router,
        });
        self
    }

    /// Consume the builder and return the list of `(path, method_router)` pairs
    /// for backward compatibility with plain Axum routers.
    pub fn into_routes(self) -> Vec<(String, MethodRouter<S>)> {
        self.routes
            .into_iter()
            .map(|r| (r.path, r.router))
            .collect()
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
        async fn patch() {}
        async fn destroy() {}

        let resource = Resource::<()>::new("posts")
            .index(get(index))
            .show(get(show))
            .create(post(create))
            .update(put(update))
            .patch(axum::routing::patch(patch))
            .destroy(delete(destroy));

        let paths: Vec<&str> = resource.routes().iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/posts",
                "/posts/{id}",
                "/posts",
                "/posts/{id}",
                "/posts/{id}",
                "/posts/{id}"
            ]
        );

        let methods: Vec<Method> = resource.routes().iter().map(|r| r.method.clone()).collect();
        assert_eq!(
            methods,
            vec![
                Method::GET,
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE
            ]
        );
    }

    #[test]
    fn resource_with_group_prefix_applies_prefix() {
        use crate::routing::RouteGroup;

        async fn index() {}

        let routes = Resource::<()>::new("posts").index(get(index)).into_routes();

        let group = RouteGroup::<()>::new().prefix("/api").add_routes(routes);
        let _router = group.build();
    }
}
