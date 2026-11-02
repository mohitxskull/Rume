//! Automatic OpenAPI v3.1 and JSON Schema generation.
//!
//! Generates OpenAPI specifications directly from the canonical [`RouteRegistry`](crate::routing::RouteRegistry),
//! reflecting path parameters, HTTP methods, operation names, and parameter schemas.

pub mod spec;

pub use spec::{
    OpenApiInfo, OpenApiOperation, OpenApiParameter, OpenApiParameterIn, OpenApiPathItem,
    OpenApiResponses, OpenApiSchema, OpenApiSpec, generate_openapi,
};
