//! OpenAPI specification data structures and generator.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::routing::registry::{RouteRegistry, extract_path_params};

/// API metadata for the OpenAPI document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApiInfo {
    /// Title of the API.
    pub title: String,
    /// API version string (e.g. "1.0.0").
    pub version: String,
    /// Optional human-readable description of the API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl OpenApiInfo {
    /// Creates a new OpenAPI info block.
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            description: None,
        }
    }

    /// Sets the description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Parameter location in an HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenApiParameterIn {
    Path,
    Query,
    Header,
    Cookie,
}

/// JSON Schema representation for parameter types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApiSchema {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

impl OpenApiSchema {
    pub fn string() -> Self {
        Self {
            r#type: "string".into(),
            format: None,
            pattern: None,
        }
    }

    pub fn integer() -> Self {
        Self {
            r#type: "integer".into(),
            format: Some("int64".into()),
            pattern: None,
        }
    }

    pub fn uuid() -> Self {
        Self {
            r#type: "string".into(),
            format: Some("uuid".into()),
            pattern: None,
        }
    }
}

/// An OpenAPI parameter definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApiParameter {
    pub name: String,
    pub r#in: OpenApiParameterIn,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema: OpenApiSchema,
}

/// Standard response definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApiResponse {
    pub description: String,
}

pub type OpenApiResponses = BTreeMap<String, OpenApiResponse>;

/// Operation object describing an endpoint and method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OpenApiOperation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<OpenApiParameter>,
    pub responses: OpenApiResponses,
}

/// Path item object describing the operations on a single path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OpenApiPathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<OpenApiOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<OpenApiOperation>,
}

/// Root OpenAPI v3.1 specification document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: BTreeMap<String, OpenApiPathItem>,
}

impl OpenApiSpec {
    /// Serializes this OpenAPI specification into pretty-printed JSON.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Converts this OpenAPI specification into a `serde_json::Value`.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// Generates a complete OpenAPI v3.1 document from a canonical [`RouteRegistry`].
pub fn generate_openapi(registry: &RouteRegistry, info: OpenApiInfo) -> OpenApiSpec {
    let mut paths: BTreeMap<String, OpenApiPathItem> = BTreeMap::new();

    for route in registry.routes() {
        // Standardize path: convert `:id` to `{id}` if present
        let mut openapi_path = route.path.clone();
        for param in extract_path_params(&route.path) {
            let colon_form = format!(":{param}");
            let brace_form = format!("{{{param}}}");
            openapi_path = openapi_path.replace(&colon_form, &brace_form);
        }

        let mut parameters = Vec::new();
        for param in &route.params {
            parameters.push(OpenApiParameter {
                name: param.clone(),
                r#in: OpenApiParameterIn::Path,
                required: true,
                description: None,
                schema: if param.contains("uuid") {
                    OpenApiSchema::uuid()
                } else if param.contains("id") || param.contains("count") {
                    OpenApiSchema::integer()
                } else {
                    OpenApiSchema::string()
                },
            });
        }

        let mut responses = BTreeMap::new();
        responses.insert(
            "200".to_string(),
            OpenApiResponse {
                description: "Successful response".to_string(),
            },
        );
        responses.insert(
            "400".to_string(),
            OpenApiResponse {
                description: "Bad Request (RFC 9457 Problem Details)".to_string(),
            },
        );
        responses.insert(
            "422".to_string(),
            OpenApiResponse {
                description: "Validation Failed (RFC 9457 Problem Details)".to_string(),
            },
        );

        let operation = OpenApiOperation {
            operation_id: route.name.clone(),
            summary: route.name.clone(),
            parameters,
            responses,
        };

        let path_item = paths.entry(openapi_path).or_default();
        match route.method.as_str() {
            "GET" => path_item.get = Some(operation),
            "POST" => path_item.post = Some(operation),
            "PUT" => path_item.put = Some(operation),
            "DELETE" => path_item.delete = Some(operation),
            "PATCH" => path_item.patch = Some(operation),
            _ => {}
        }
    }

    OpenApiSpec {
        openapi: "3.1.0".to_string(),
        info,
        paths,
    }
}
