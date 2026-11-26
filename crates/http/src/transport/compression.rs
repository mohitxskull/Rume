//! HTTP response compression (gzip, brotli, zstd) backed by `tower-http`.

use tower_http::compression::predicate::{And, NotForContentType, Predicate, SizeAbove};

/// Default minimum response size in bytes eligible for compression (1 KiB).
///
/// Why: compressing payloads smaller than ~1 KiB typically increases wire size
/// (header + framing overhead) and wastes CPU without reducing TCP packet count;
/// matches common reverse-proxy thresholds while remaining configurable down to
/// `0` bytes for tests or small-payload endpoints.
pub const DEFAULT_COMPRESSION_MIN_SIZE_BYTES: u16 = 1024;

/// Compression predicate combining a configurable minimum body size with
/// `tower-http`'s standard exclusions (gRPC, images, and Server-Sent Events).
pub type CompressionPredicate =
    And<And<And<SizeAbove, NotForContentType>, NotForContentType>, NotForContentType>;

/// Tower layer compressing HTTP responses according to `Accept-Encoding`.
pub type CompressionLayer = tower_http::compression::CompressionLayer<CompressionPredicate>;

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
            min_size_bytes: DEFAULT_COMPRESSION_MIN_SIZE_BYTES,
        }
    }
}

impl CompressionConfig {
    /// Creates a compression configuration with gzip, brotli, and zstd enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables or disables gzip response compression.
    pub fn with_gzip(mut self, enabled: bool) -> Self {
        self.gzip = enabled;
        self
    }

    /// Enables or disables brotli response compression.
    pub fn with_brotli(mut self, enabled: bool) -> Self {
        self.brotli = enabled;
        self
    }

    /// Enables or disables zstd response compression.
    pub fn with_zstd(mut self, enabled: bool) -> Self {
        self.zstd = enabled;
        self
    }

    /// Sets the minimum response body size in bytes eligible for compression.
    pub fn with_min_size_bytes(mut self, min_size_bytes: u16) -> Self {
        self.min_size_bytes = min_size_bytes;
        self
    }

    /// Builds a configured [`CompressionLayer`].
    pub fn build_layer(&self) -> CompressionLayer {
        let predicate = SizeAbove::new(self.min_size_bytes)
            .and(NotForContentType::GRPC)
            .and(NotForContentType::IMAGES)
            .and(NotForContentType::SSE);

        tower_http::compression::CompressionLayer::new()
            .gzip(self.gzip)
            .br(self.brotli)
            .zstd(self.zstd)
            .compress_when(predicate)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, Response, StatusCode, header};
    use axum::routing::get;
    use tower::{Layer, ServiceExt};
    use tower_http::decompression::DecompressionLayer;

    use crate::transport::compression::CompressionConfig;

    async fn decompress_response(response: Response<Body>, accept_encoding: &str) -> Vec<u8> {
        let mut response = Some(response);
        let svc = DecompressionLayer::new().layer(tower::service_fn(move |_req: Request<Body>| {
            let resp = response.take().expect("response already consumed");
            async move { Ok::<_, Infallible>(resp) }
        }));

        let decompressed = svc
            .oneshot(
                Request::builder()
                    .header(header::ACCEPT_ENCODING, accept_encoding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        to_bytes(Body::new(decompressed.into_body()), 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn compression_encodes_gzip_brotli_and_zstd_when_accepted() {
        let payload = "rume-compressible-payload-".repeat(128);
        assert!(payload.len() > 1024);

        let expected = payload.clone();
        let layer = CompressionConfig::new().build_layer();

        for encoding in ["gzip", "br", "zstd"] {
            let body_text = payload.clone();
            let app = Router::new()
                .route("/data", get(move || async move { body_text }))
                .layer(layer.clone());

            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/data")
                        .header(header::ACCEPT_ENCODING, encoding)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(header::CONTENT_ENCODING),
                Some(&header::HeaderValue::from_str(encoding).unwrap()),
                "expected Content-Encoding: {encoding}"
            );
            let vary = response
                .headers()
                .get(header::VARY)
                .expect("missing Vary header")
                .to_str()
                .unwrap()
                .to_ascii_lowercase();
            assert!(
                vary.contains("accept-encoding"),
                "expected Vary to contain accept-encoding, got: {vary}"
            );

            let decoded = decompress_response(response, encoding).await;
            assert_eq!(
                decoded,
                expected.as_bytes(),
                "decompressed {encoding} payload mismatch"
            );
        }
    }

    #[tokio::test]
    async fn compression_skips_small_responses_below_threshold() {
        let app = Router::new()
            .route("/tiny", get(|| async { "small" }))
            .layer(CompressionConfig::new().build_layer());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/tiny")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::CONTENT_ENCODING).is_none(),
            "tiny response below min_size_bytes must not be compressed"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), b"small");
    }

    #[tokio::test]
    async fn compression_respects_disabled_algorithms() {
        let payload = "rume-zstd-disabled-payload-".repeat(128);
        let expected = payload.clone();

        let app = Router::new()
            .route("/data", get(move || async move { payload }))
            .layer(CompressionConfig::new().with_zstd(false).build_layer());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/data")
                    .header(header::ACCEPT_ENCODING, "zstd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::CONTENT_ENCODING).is_none(),
            "disabled zstd algorithm must not be used"
        );
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(body.as_ref(), expected.as_bytes());
    }
}
