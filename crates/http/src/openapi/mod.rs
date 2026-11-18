//! Automatic OpenAPI v3.1 and JSON Schema generation.
//!
//! Generates OpenAPI specifications from any [`OpenApiRouteSource`],
//! reflecting path parameters, HTTP methods, operation names, and parameter schemas.

pub mod spec;

pub use spec::{
    HasOpenApiSchema, OpenApiInfo, OpenApiOperation, OpenApiParamKind, OpenApiParameter,
    OpenApiParameterIn, OpenApiPathItem, OpenApiResponses, OpenApiRoute, OpenApiRouteSource,
    OpenApiSchema, OpenApiSpec, generate_openapi,
};
