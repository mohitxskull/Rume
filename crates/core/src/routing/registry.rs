//! Route information and registry.
//!
//! Provides [`RouteInfo`] and the application-scoped [`RouteRegistry`] used
//! by [`crate::routing::RouteSpec`] and [`crate::routing::Router`] when routes
//! are mounted.

use axum::http::Method;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::routing::matchers::Matcher;

mod method_serde {
    use super::*;

    pub fn serialize<S>(method: &Method, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(method.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Method, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Method::from_bytes(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

/// Information about a registered route (for `rume list:routes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteInfo {
    #[serde(with = "method_serde")]
    pub method: Method,
    pub path: String,
    pub name: Option<String>,
    pub params: Vec<String>,
    #[serde(default)]
    pub matchers: Vec<(String, Matcher)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_schema: Option<crate::openapi::spec::OpenApiSchema>,
}

impl RouteInfo {
    /// Create a new `RouteInfo` without matchers.
    pub fn new(
        method: Method,
        path: impl Into<String>,
        name: Option<String>,
        params: Vec<String>,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            name,
            params,
            matchers: Vec::new(),
            request_schema: None,
        }
    }

    /// Attach declared parameter matchers.
    pub fn with_matchers(mut self, matchers: Vec<(String, Matcher)>) -> Self {
        self.matchers = matchers;
        self
    }

    /// Attach an OpenAPI request schema.
    pub fn with_request_schema(mut self, schema: crate::openapi::spec::OpenApiSchema) -> Self {
        self.request_schema = Some(schema);
        self
    }
}

/// Application-scoped route registry. Each Rume application owns its own
/// registry; route metadata is tied to the application that registered it,
/// not to a process-global singleton.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    /// Append all routes from another registry.
    pub fn extend(&mut self, other: RouteRegistry) {
        self.routes.extend(other.routes);
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

    /// Generate an OpenAPI v3.1 document reflecting all routes in this registry.
    pub fn to_openapi(&self, info: crate::openapi::OpenApiInfo) -> crate::openapi::OpenApiSpec {
        crate::openapi::generate_openapi(self, info)
    }
}

/// Extract path parameter names from a route template string (e.g. `/{id}` or `/:id`).
pub fn extract_path_params(path: &str) -> Vec<String> {
    let mut params = Vec::new();
    for segment in path.split('/') {
        let segment = segment.trim();
        if segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2 {
            params.push(segment[1..segment.len() - 1].to_string());
        } else if let Some(stripped) = segment.strip_prefix(':').filter(|s| !s.is_empty()) {
            params.push(stripped.to_string());
        }
    }
    params
}

/// Join a prefix and path with exactly one `/` separator.
///
/// Handles leading/trailing slashes on both sides so that
/// `join_path("/api/", "/users")` → `/api/users`,
/// `join_path("api", "users")` → `/api/users`, and
/// `join_path("", "/users")` → `/users`.
pub fn join_path(prefix: &str, path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_registry_register_and_list() {
        let mut registry = RouteRegistry::new();
        assert!(registry.is_empty());

        registry.register(RouteInfo {
            method: Method::GET,
            path: "/users".to_string(),
            name: Some("users.index".to_string()),
            params: vec![],
            matchers: vec![],
            request_schema: None,
        });
        registry.register(RouteInfo {
            method: Method::POST,
            path: "/users".to_string(),
            name: Some("users.create".to_string()),
            params: vec![],
            matchers: vec![],
            request_schema: None,
        });

        assert_eq!(registry.len(), 2);
        let routes = registry.routes();
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].method, Method::GET);
        assert_eq!(routes[1].method, Method::POST);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn extracts_path_parameters() {
        assert_eq!(extract_path_params("/users"), Vec::<String>::new());
        assert_eq!(extract_path_params("/users/{id}"), vec!["id"]);
        assert_eq!(extract_path_params("/users/:id"), vec!["id"]);
        assert_eq!(
            extract_path_params("/orgs/{org_id}/teams/:team_id"),
            vec!["org_id", "team_id"]
        );
    }

    #[test]
    fn joins_paths_cleanly() {
        assert_eq!(join_path("/api/", "/users"), "/api/users");
        assert_eq!(join_path("api", "users"), "/api/users");
        assert_eq!(join_path("", "/users"), "/users");
        assert_eq!(join_path("/v1", ""), "/v1");
    }
}
