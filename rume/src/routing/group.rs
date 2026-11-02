//! Route groups.
//!
//! Provides [`RouteGroup`], a builder for applying shared configuration
//! (URL prefix) to multiple routes before mounting them on a [`Router`].

use axum::handler::Handler;
use axum::http::Method;
use axum::routing::MethodRouter;

use crate::routing::registry::{join_path, RouteInfo};
use crate::routing::resource::Resource;
use crate::routing::router::Router;
use crate::routing::spec::RouteSpec;

/// Route group builder for applying shared configuration to multiple routes.
pub struct RouteGroup<S> {
    router: Router<S>,
    prefix: String,
}

impl<S: Clone + Send + Sync + 'static> RouteGroup<S> {
    /// Create a new route group.
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            prefix: String::new(),
        }
    }

    /// Set a URL prefix for all routes in the group.
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.to_string();
        self
    }

    /// Mount a `GET` route under this group's prefix.
    pub fn get<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let full_path = join_path(&self.prefix, path);
        self.router = self.router.get(&full_path, handler);
        self
    }

    /// Mount a `POST` route under this group's prefix.
    pub fn post<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let full_path = join_path(&self.prefix, path);
        self.router = self.router.post(&full_path, handler);
        self
    }

    /// Mount a `PUT` route under this group's prefix.
    pub fn put<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let full_path = join_path(&self.prefix, path);
        self.router = self.router.put(&full_path, handler);
        self
    }

    /// Mount a `PATCH` route under this group's prefix.
    pub fn patch<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let full_path = join_path(&self.prefix, path);
        self.router = self.router.patch(&full_path, handler);
        self
    }

    /// Mount a `DELETE` route under this group's prefix.
    pub fn delete<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let full_path = join_path(&self.prefix, path);
        self.router = self.router.delete(&full_path, handler);
        self
    }

    /// Add a route method router to the group.
    pub fn add(mut self, path: &str, method_router: MethodRouter<S>) -> Self {
        let full_path = join_path(&self.prefix, path);
        self.router = self.router.route(&full_path, method_router);
        self
    }

    /// Mount a [`RouteSpec`] under this group's prefix.
    pub fn route_spec<H, T>(mut self, spec: RouteSpec, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let full_path = join_path(&self.prefix, spec.path());
        let mut new_spec = match *spec.method() {
            Method::GET => RouteSpec::get(&full_path),
            Method::POST => RouteSpec::post(&full_path),
            Method::PUT => RouteSpec::put(&full_path),
            Method::PATCH => RouteSpec::patch(&full_path),
            Method::DELETE => RouteSpec::delete(&full_path),
            _ => RouteSpec::get(&full_path),
        };
        for (param, matcher) in spec.params() {
            new_spec = new_spec.where_param(param, matcher.clone());
        }
        if let Some(name) = spec.name() {
            new_spec = new_spec.r#as(name);
        }
        self.router = self.router.route_spec(new_spec, handler);
        self
    }

    /// Mount a [`Resource`] under this group's prefix.
    pub fn resource(mut self, resource: Resource<S>) -> Self {
        for r in resource.routes() {
            let full_path = join_path(&self.prefix, &r.path);
            let mut sub_router = Router::new().route(&full_path, r.router.clone());
            sub_router.registry_mut().clear();
            sub_router.registry_mut().register(RouteInfo {
                method: r.method.clone(),
                path: full_path,
                name: Some(r.name.clone()),
                params: r.params.clone(),
            });
            self.router = self.router.merge(sub_router);
        }
        self
    }

    /// Add multiple `(path, method_router)` routes to the group.
    pub fn add_routes(
        mut self,
        routes: impl IntoIterator<Item = (String, MethodRouter<S>)>,
    ) -> Self {
        for (path, router) in routes.into_iter() {
            let full_path = join_path(&self.prefix, &path);
            self.router = self.router.route(&full_path, router);
        }
        self
    }

    /// Build into a [`Router`].
    pub fn build(self) -> Router<S> {
        self.router
    }
}

impl<S: Clone + Send + Sync + 'static> Default for RouteGroup<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone + Send + Sync + 'static> From<RouteGroup<S>> for Router<S> {
    fn from(group: RouteGroup<S>) -> Self {
        group.build()
    }
}

impl<S: Clone + Send + Sync + 'static> From<RouteGroup<S>> for axum::Router<S> {
    fn from(group: RouteGroup<S>) -> Self {
        group.build().into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn dummy_handler() -> &'static str {
        "ok"
    }

    #[test]
    fn route_group_applies_prefix() {
        let group = RouteGroup::<()>::new()
            .prefix("/api/v1")
            .get("/users", dummy_handler)
            .get("/users/{id}", dummy_handler);

        let router = group.build();
        assert_eq!(router.routes().len(), 2);
        assert_eq!(router.routes()[0].path, "/api/v1/users");
        assert_eq!(router.routes()[1].path, "/api/v1/users/{id}");
        assert_eq!(router.routes()[1].params, vec!["id"]);
    }
}
