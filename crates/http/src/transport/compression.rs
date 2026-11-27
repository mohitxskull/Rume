//! HTTP response compression (gzip, brotli, zstd) backed by `tower-http`.

use axum::body::HttpBody;
use axum::http::{Response, header};
use tower_http::compression::predicate::{And, NotForContentType, Predicate, SizeAbove};

/// Default minimum response size in bytes eligible for compression (1 KiB).
///
/// Why: compressing payloads smaller than ~1 KiB typically increases wire size
/// (header + framing overhead) and wastes CPU without reducing TCP packet count;
/// matches common reverse-proxy thresholds while remaining configurable down to
/// `0` bytes for tests or small-payload endpoints.
pub const DEFAULT_COMPRESSION_MIN_SIZE_BYTES: u16 = 1024;

/// Compression predicate that skips responses carrying `Set-Cookie` or
/// `Cache-Control: no-store` / `private` as a default-on BREACH defense over TLS (`RC-41`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SkipSensitiveHeaders;

impl Predicate for SkipSensitiveHeaders {
    fn should_compress<B>(&self, response: &Response<B>) -> bool
    where
        B: HttpBody,
    {
        if response.headers().contains_key(header::SET_COOKIE) {
            return false;
        }
        for val in response.headers().get_all(header::CACHE_CONTROL) {
            if let Ok(s) = val.to_str() {
                for directive in s.split(',') {
                    let d = directive.trim().to_ascii_lowercase();
                    if d == "no-store" || d == "private" || d.starts_with("private=") {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// Compression predicate combining a configurable minimum body size,
/// `tower-http`'s standard exclusions (gRPC, images, and Server-Sent Events),
/// and [`SkipSensitiveHeaders`] BREACH protection.
pub type CompressionPredicate = And<
    And<And<And<SizeAbove, NotForContentType>, NotForContentType>, NotForContentType>,
    SkipSensitiveHeaders,
>;

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
        let predicate = SizeAbove::new(u64::from(self.min_size_bytes))
            .and(NotForContentType::GRPC)
            .and(NotForContentType::IMAGES)
            .and(NotForContentType::SSE)
            .and(SkipSensitiveHeaders);

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

    #[tokio::test]
    async fn compression_skips_set_cookie_and_private_or_no_store_responses_for_breach_defense() {
        let payload = "rume-sensitive-secret-payload-".repeat(128);
        assert!(payload.len() > 1024);
        let layer = CompressionConfig::new().build_layer();

        let p1 = payload.clone();
        let p2 = payload.clone();
        let p3 = payload.clone();
        let app = Router::new()
            .route(
                "/cookie",
                get(move || async move { ([(header::SET_COOKIE, "sid=abc; HttpOnly")], p1) }),
            )
            .route(
                "/private",
                get(move || async move { ([(header::CACHE_CONTROL, "private, max-age=60")], p2) }),
            )
            .route(
                "/no-store",
                get(move || async move { ([(header::CACHE_CONTROL, "no-store")], p3) }),
            )
            .layer(layer);

        for uri in ["/cookie", "/private", "/no-store"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::ACCEPT_ENCODING, "gzip")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers().get(header::CONTENT_ENCODING).is_none(),
                "response at {uri} must not be compressed (BREACH defense)"
            );
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            assert_eq!(body.as_ref(), payload.as_bytes());
        }
    }
}
