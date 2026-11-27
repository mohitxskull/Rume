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

    /// Sets the maximum permitted request body size in bytes.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Builds a [`BodyLimitLayer`] enforcing `self.max_bytes`.
    pub fn build_layer(&self) -> BodyLimitLayer {
        tower_http::limit::RequestBodyLimitLayer::new(self.max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, Bytes, HttpBody};
    use axum::http::{Method, Request, StatusCode, header};
    use axum::routing::post;
    use tower::ServiceExt;

    use crate::transport::body_limit::BodyLimitConfig;

    #[tokio::test]
    async fn body_limit_allows_payload_within_limit_and_rejects_oversized_with_413() {
        let limit = 16;
        let app = Router::new()
            .route(
                "/upload",
                post(|body: Bytes| async move { format!("{}", body.len()) }),
            )
            .layer(BodyLimitConfig::new(limit).build_layer());

        // Exact boundary: max_bytes succeeds.
        let ok_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/upload")
                    .header(header::CONTENT_LENGTH, limit.to_string())
                    .body(Body::from(vec![b'a'; limit]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_response.status(), StatusCode::OK);

        // One byte over limit: 413 Payload Too Large.
        let oversized_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/upload")
                    .header(header::CONTENT_LENGTH, (limit + 1).to_string())
                    .body(Body::from(vec![b'a'; limit + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(oversized_response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn body_limit_rejects_streaming_body_without_content_length() {
        let limit = 16;
        let app = Router::new()
            .route(
                "/upload",
                post(|body: Bytes| async move { format!("{}", body.len()) }),
            )
            .layer(BodyLimitConfig::new(limit).build_layer());

        // Streamed body with no Content-Length header and unknown size_hint exceeding limit.
        let streamed = Body::from_stream(Body::from(vec![b'x'; limit + 1]).into_data_stream());
        assert!(
            streamed.size_hint().exact().is_none(),
            "streamed body must not expose an exact size hint"
        );

        let request = Request::builder()
            .method(Method::POST)
            .uri("/upload")
            .body(streamed)
            .unwrap();
        assert!(request.headers().get(header::CONTENT_LENGTH).is_none());

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
