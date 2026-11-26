//! OpenAPI documentation UI route helpers supporting Scalar and Swagger UI.

use crate::routing::Router;

/// Selectable documentation UI renderer for OpenAPI specifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenApiUi {
    /// Scalar API Reference UI (default).
    #[default]
    Scalar,
    /// Swagger UI bundle.
    SwaggerUi,
}

/// Configuration for serving an OpenAPI documentation UI page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiDocsConfig {
    /// HTML document `<title>` (default: `"API Documentation"`).
    pub title: String,
    /// URL of the OpenAPI JSON specification (default: `"/openapi.json"`).
    pub spec_url: String,
    /// Documentation UI renderer (default: [`OpenApiUi::Scalar`]).
    pub ui: OpenApiUi,
    /// Optional override for the UI script URL (for self-hosted or air-gapped deployments).
    pub script_url: Option<String>,
    /// Optional override for the Swagger UI stylesheet URL (unused for Scalar).
    pub stylesheet_url: Option<String>,
}

impl Default for OpenApiDocsConfig {
    fn default() -> Self {
        Self {
            title: "API Documentation".to_string(),
            spec_url: "/openapi.json".to_string(),
            ui: OpenApiUi::default(),
            script_url: None,
            stylesheet_url: None,
        }
    }
}

impl OpenApiDocsConfig {
    /// Creates an OpenAPI docs configuration pointing at `spec_url`.
    #[allow(unused_variables, dead_code)] // Removed in Task 06.
    pub fn new(spec_url: impl Into<String>) -> Self {
        todo!("implemented in Task 06")
    }
}

/// Renders a safe HTML5 document for the configured OpenAPI documentation UI.
#[allow(unused_variables, dead_code)] // Removed in Task 06.
pub fn openapi_docs_html(config: &OpenApiDocsConfig) -> String {
    todo!("implemented in Task 06")
}

/// Mounts a `GET` route serving the configured OpenAPI documentation UI at `docs_path`.
#[allow(unused_variables, dead_code)] // Removed in Task 06.
pub fn openapi_docs_routes<S>(docs_path: &str, config: OpenApiDocsConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    todo!("implemented in Task 06")
}
