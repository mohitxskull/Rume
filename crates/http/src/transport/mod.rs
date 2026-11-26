//! HTTP transport middleware: CORS, response compression, and request body-size limits.

pub mod body_limit;
pub mod compression;
pub mod cors;

pub use body_limit::{BodyLimitConfig, BodyLimitLayer, DEFAULT_BODY_LIMIT_BYTES};
pub use compression::{CompressionConfig, CompressionLayer};
pub use cors::{CorsConfig, CorsConfigError, CorsLayer};
