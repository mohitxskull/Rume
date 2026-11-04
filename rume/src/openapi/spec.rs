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

/// JSON Schema representation for parameter types and request/response payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OpenApiSchema {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "minLength")]
    pub min_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxLength")]
    pub max_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, OpenApiSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<OpenApiSchema>>,
}

impl OpenApiSchema {
    pub fn string() -> Self {
        Self {
            r#type: "string".into(),
            ..Default::default()
        }
    }

    pub fn integer() -> Self {
        Self {
            r#type: "integer".into(),
            format: Some("int64".into()),
            ..Default::default()
        }
    }

    pub fn number() -> Self {
        Self {
            r#type: "number".into(),
            ..Default::default()
        }
    }

    pub fn boolean() -> Self {
        Self {
            r#type: "boolean".into(),
            ..Default::default()
        }
    }

    pub fn object() -> Self {
        Self {
            r#type: "object".into(),
            ..Default::default()
        }
    }

    pub fn uuid() -> Self {
        Self {
            r#type: "string".into(),
            format: Some("uuid".into()),
            ..Default::default()
        }
    }

    pub fn array(items: OpenApiSchema) -> Self {
        Self {
            r#type: "array".into(),
            items: Some(Box::new(items)),
            ..Default::default()
        }
    }

    pub fn with_min_length(mut self, min: usize) -> Self {
        self.min_length = Some(min);
        self
    }

    pub fn with_max_length(mut self, max: usize) -> Self {
        self.max_length = Some(max);
        self
    }

    pub fn with_range(mut self, min: usize, max: usize) -> Self {
        self.min_length = Some(min);
        self.max_length = Some(max);
        self
    }

    pub fn with_minimum(mut self, min: i64) -> Self {
        self.minimum = Some(min);
        self
    }

    pub fn with_maximum(mut self, max: i64) -> Self {
        self.maximum = Some(max);
        self
    }

    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = Some(format.into());
        self
    }

    pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.pattern = Some(pattern.into());
        self
    }

    pub fn with_property(mut self, name: impl Into<String>, schema: OpenApiSchema) -> Self {
        let mut props = self.properties.unwrap_or_default();
        props.insert(name.into(), schema);
        self.properties = Some(props);
        self
    }

    pub fn with_required_property(mut self, name: impl Into<String>, schema: OpenApiSchema) -> Self {
        let name_str = name.into();
        let mut req = self.required.unwrap_or_default();
        if !req.contains(&name_str) {
            req.push(name_str.clone());
        }
        self.required = Some(req);
        self.with_property(name_str, schema)
    }
}

/// Trait for types that can describe their OpenAPI JSON Schema representation,
/// including field constraints and garde validation rules.
pub trait HasOpenApiSchema {
    /// Returns the OpenAPI schema representation.
    fn schema() -> OpenApiSchema;
}

impl HasOpenApiSchema for String {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::string()
    }
}

impl HasOpenApiSchema for &str {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::string()
    }
}

impl HasOpenApiSchema for i8 { fn schema() -> OpenApiSchema { OpenApiSchema::integer() } }
impl HasOpenApiSchema for i16 { fn schema() -> OpenApiSchema { OpenApiSchema::integer() } }
impl HasOpenApiSchema for i32 { fn schema() -> OpenApiSchema { OpenApiSchema::integer() } }
impl HasOpenApiSchema for i64 { fn schema() -> OpenApiSchema { OpenApiSchema::integer() } }
impl HasOpenApiSchema for isize { fn schema() -> OpenApiSchema { OpenApiSchema::integer() } }
impl HasOpenApiSchema for u8 { fn schema() -> OpenApiSchema { OpenApiSchema::integer().with_minimum(0) } }
impl HasOpenApiSchema for u16 { fn schema() -> OpenApiSchema { OpenApiSchema::integer().with_minimum(0) } }
impl HasOpenApiSchema for u32 { fn schema() -> OpenApiSchema { OpenApiSchema::integer().with_minimum(0) } }
impl HasOpenApiSchema for u64 { fn schema() -> OpenApiSchema { OpenApiSchema::integer().with_minimum(0) } }
impl HasOpenApiSchema for usize { fn schema() -> OpenApiSchema { OpenApiSchema::integer().with_minimum(0) } }
impl HasOpenApiSchema for f32 { fn schema() -> OpenApiSchema { OpenApiSchema::number() } }
impl HasOpenApiSchema for f64 { fn schema() -> OpenApiSchema { OpenApiSchema::number() } }
impl HasOpenApiSchema for bool { fn schema() -> OpenApiSchema { OpenApiSchema::boolean() } }
impl HasOpenApiSchema for uuid::Uuid { fn schema() -> OpenApiSchema { OpenApiSchema::uuid() } }

impl<T: HasOpenApiSchema> HasOpenApiSchema for Vec<T> {
    fn schema() -> OpenApiSchema {
        OpenApiSchema::array(T::schema())
    }
}

impl<T: HasOpenApiSchema> HasOpenApiSchema for Option<T> {
    fn schema() -> OpenApiSchema {
        T::schema()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<serde_json::Value>,
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

/// OpenAPI Components object for schemas and security schemes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OpenApiComponents {
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub schemas: BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub security_schemes: BTreeMap<String, serde_json::Value>,
}

/// Root OpenAPI v3.1 specification document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub paths: BTreeMap<String, OpenApiPathItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<OpenApiComponents>,
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
            let schema = if let Some((_, matcher)) = route.matchers.iter().find(|(p, _)| p == param)
            {
                match matcher {
                    crate::routing::Matcher::Number => OpenApiSchema::integer().with_minimum(0),
                    crate::routing::Matcher::Uuid => OpenApiSchema::uuid(),
                    crate::routing::Matcher::Slug => OpenApiSchema {
                        r#type: "string".into(),
                        format: None,
                        pattern: Some("^[a-zA-Z0-9][a-zA-Z0-9_-]{1,}[a-zA-Z0-9]$".into()),
                        min_length: Some(3),
                        ..Default::default()
                    },
                }
            } else if param.contains("uuid") {
                OpenApiSchema::uuid()
            } else if param.contains("id") || param.contains("count") {
                OpenApiSchema::integer()
            } else {
                OpenApiSchema::string()
            };

            parameters.push(OpenApiParameter {
                name: param.clone(),
                r#in: OpenApiParameterIn::Path,
                required: true,
                description: None,
                schema,
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

        let request_body = route.request_schema.as_ref().map(|schema| {
            serde_json::json!({
                "description": "Request payload",
                "required": true,
                "content": {
                    "application/json": {
                        "schema": schema
                    }
                }
            })
        });

        let operation = OpenApiOperation {
            operation_id: route.name.clone(),
            summary: route.name.clone(),
            parameters,
            request_body,
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
        components: None,
    }
}
