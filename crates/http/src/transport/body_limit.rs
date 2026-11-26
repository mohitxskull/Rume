//! Transport-level HTTP request body-size limit backed by `tower-http`.

/// Default maximum request body size in bytes (2 MiB).
///
/// Why: matches Axum's `DefaultBodyLimit` default while enforcing the cap at the
/// HTTP transport layer (`413 Payload Too Large`) before handlers stream
/// unbounded request bodies.
pub const DEFAULT_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Tower layer enforcing a maximum request body size in bytes.
pub type BodyLimitLayer = tower_http::limit::RequestBodyLimitLayer;

/// Configuration for transport-level HTTP request body-size limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyLimitConfig {
    /// Maximum permitted request body size in bytes.
    pub max_bytes: usize,
}

impl Default for BodyLimitConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_BODY_LIMIT_BYTES,
        }
    }
}

impl BodyLimitConfig {
    /// Creates a body-limit configuration with the given maximum byte count.
    pub fn new(max_bytes: usize) -> Self {
        Self { max_bytes }
    }

    /// Builds a [`BodyLimitLayer`] enforcing `self.max_bytes`.
    #[allow(unused_variables, dead_code)] // Removed in Task 02.
    pub fn build_layer(&self) -> BodyLimitLayer {
        todo!("implemented in Task 02")
    }
}
