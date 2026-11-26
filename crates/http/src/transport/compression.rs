//! HTTP response compression (gzip, brotli, zstd) backed by `tower-http`.

/// Tower layer compressing HTTP responses according to `Accept-Encoding`.
pub type CompressionLayer = tower_http::compression::CompressionLayer;

/// Configuration for HTTP response compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionConfig {
    /// Enable gzip response compression (default: `true`).
    pub gzip: bool,
    /// Enable brotli response compression (default: `true`).
    pub brotli: bool,
    /// Enable zstd response compression (default: `true`).
    pub zstd: bool,
    /// Minimum response body size in bytes eligible for compression (default: `1024`).
    pub min_size_bytes: u16,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            gzip: true,
            brotli: true,
            zstd: true,
            min_size_bytes: 1024,
        }
    }
}

impl CompressionConfig {
    /// Creates a compression configuration with gzip, brotli, and zstd enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a configured [`CompressionLayer`].
    #[allow(unused_variables, dead_code)] // Removed in Task 02.
    pub fn build_layer(&self) -> CompressionLayer {
        todo!("implemented in Task 02")
    }
}
