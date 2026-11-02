//! Route information and registry.
//!
//! Provides [`RouteInfo`] and the application-scoped [`RouteRegistry`] used
//! by [`crate::routing::RouteSpec`] metadata when routes are mounted.

use axum::http::Method;

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn route_registry_register_and_list() {
        let mut registry = RouteRegistry::new();
        assert!(registry.is_empty());

        registry.register(RouteInfo {
            method: Method::GET,
            path: "/users".to_string(),
            name: Some("users.index".to_string()),
            params: vec![],
        });
        registry.register(RouteInfo {
            method: Method::POST,
            path: "/users".to_string(),
            name: Some("users.create".to_string()),
            params: vec![],
        });

        assert_eq!(registry.len(), 2);
        let routes = registry.routes();
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].method, Method::GET);
        assert_eq!(routes[1].method, Method::POST);

        registry.clear();
        assert!(registry.is_empty());
    }
}
