//! HTTP response caching and conditional request middleware for Rume.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rume_data::cache::CacheBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower::{Layer, Service};

/// Maximum response body size buffered for caching / ETag hashing (8 MiB).
const MAX_CACHEABLE_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Configuration for [`HttpCacheLayer`].
#[derive(Debug, Clone)]
pub struct HttpCacheConfig {
    /// Time-to-live for cached responses in the backend.
    pub ttl: Duration,
    /// Optional `Cache-Control` header value applied to cacheable responses.
    pub cache_control: Option<String>,
    /// Whether generated `ETag`s are weak (`W/"..."`) instead of strong (`"..."`).
    pub weak_etags: bool,
    /// Request header names included in the cache key and emitted in `Vary`.
    pub vary_headers: Vec<HeaderName>,
}

impl Default for HttpCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            cache_control: Some("public, max-age=60".to_string()),
            weak_etags: false,
            vary_headers: Vec::new(),
        }
    }
}

impl HttpCacheConfig {
    /// Creates a new [`HttpCacheConfig`] with default settings (60s TTL).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the cache backend TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Sets the `Cache-Control` header value.
    pub fn with_cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }

    /// Disables automatic `Cache-Control` header injection.
    pub fn without_cache_control(mut self) -> Self {
        self.cache_control = None;
        self
    }

    /// Enables or disables weak `ETag` generation (`W/"..."`).
    pub fn with_weak_etags(mut self, weak: bool) -> Self {
        self.weak_etags = weak;
        self
    }

    /// Sets request headers that vary the cache key and `Vary` response header.
    pub fn with_vary_headers(mut self, headers: Vec<HeaderName>) -> Self {
        self.vary_headers = headers;
        self
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedHttpResponse {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    etag: String,
}

/// Tower layer providing HTTP response caching backed by [`CacheBackend`]
/// and conditional request handling (`ETag`, `If-None-Match`, `If-Match`).
#[derive(Clone)]
pub struct HttpCacheLayer {
    backend: Arc<dyn CacheBackend>,
    config: HttpCacheConfig,
}

impl HttpCacheLayer {
    /// Creates a new [`HttpCacheLayer`] backed by `backend`.
    pub fn new(backend: Arc<dyn CacheBackend>, config: HttpCacheConfig) -> Self {
        Self { backend, config }
    }

    /// Creates a new [`HttpCacheLayer`] with [`HttpCacheConfig::default()`].
    pub fn with_defaults(backend: Arc<dyn CacheBackend>) -> Self {
        Self::new(backend, HttpCacheConfig::default())
    }
}

impl<S> Layer<S> for HttpCacheLayer {
    type Service = HttpCacheService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpCacheService {
            inner,
            backend: Arc::clone(&self.backend),
            config: self.config.clone(),
        }
    }
}

/// Tower service produced by [`HttpCacheLayer`].
#[derive(Clone)]
pub struct HttpCacheService<S> {
    inner: S,
    backend: Arc<dyn CacheBackend>,
    config: HttpCacheConfig,
}

impl<S> Service<Request<Body>> for HttpCacheService<S>
where
    S: Service<Request<Body>> + Clone + Send + 'static,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        // Why: swap `self.inner` with the clone so the polled-ready instance
        // handles this request per Tower's `Service` readiness contract.
        std::mem::swap(&mut self.inner, &mut inner);

        let backend = Arc::clone(&self.backend);
        let config = self.config.clone();

        Box::pin(async move {
            let method = req.method().clone();
            let cache_key = derive_cache_key(&req, &config.vary_headers);

            if method == Method::GET || method == Method::HEAD {
                let if_none_match = header_str(req.headers().get(header::IF_NONE_MATCH));
                if let Ok(Some(raw)) = backend.get(&cache_key).await
                    && let Ok(cached) = serde_json::from_slice::<CachedHttpResponse>(&raw)
                {
                    if let Some(ref inm) = if_none_match
                        && etag_matches_weak(inm, &cached.etag)
                    {
                        return Ok(not_modified_response(&cached.etag, &config));
                    }
                    return Ok(response_from_cached(
                        cached,
                        method == Method::HEAD,
                        &config,
                    ));
                }

                let res = inner.call(req).await?.into_response();
                if res.status() != StatusCode::OK {
                    return Ok(res);
                }

                let (mut parts, body) = res.into_parts();
                let Ok(bytes) = to_bytes(body, MAX_CACHEABLE_BODY_BYTES).await else {
                    return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                };

                let etag = parts
                    .headers
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| compute_etag(&bytes, config.weak_etags));

                if let Ok(val) = HeaderValue::from_str(&etag) {
                    parts.headers.insert(header::ETAG, val);
                }
                apply_cache_headers(&mut parts.headers, &config);

                let serialized_headers = parts
                    .headers
                    .iter()
                    .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
                    .collect();

                let cached = CachedHttpResponse {
                    status: parts.status.as_u16(),
                    headers: serialized_headers,
                    body: bytes.to_vec(),
                    etag: etag.clone(),
                };
                if let Ok(encoded) = serde_json::to_vec(&cached) {
                    let _ = backend.set(&cache_key, encoded, Some(config.ttl)).await;
                }

                if let Some(ref inm) = if_none_match
                    && etag_matches_weak(inm, &etag)
                {
                    return Ok(not_modified_response(&etag, &config));
                }

                let out_body = if method == Method::HEAD {
                    Body::empty()
                } else {
                    Body::from(bytes)
                };
                return Ok(Response::from_parts(parts, out_body));
            }

            if matches!(method, Method::PUT | Method::PATCH | Method::DELETE) {
                if let Some(if_match) = header_str(req.headers().get(header::IF_MATCH)) {
                    let matched = match backend.get(&cache_key).await {
                        Ok(Some(raw)) => serde_json::from_slice::<CachedHttpResponse>(&raw)
                            .map(|cached| etag_matches_strong(&if_match, &cached.etag))
                            .unwrap_or(false),
                        _ => false,
                    };
                    if !matched {
                        return Ok(StatusCode::PRECONDITION_FAILED.into_response());
                    }
                }

                let res = inner.call(req).await?.into_response();
                if res.status().is_success() {
                    let _ = backend.delete(&cache_key).await;
                }
                return Ok(res);
            }

            Ok(inner.call(req).await?.into_response())
        })
    }
}

/// Stateless Tower layer for `ETag` computation and `If-None-Match` (`304 Not Modified`)
/// on `GET`/`HEAD` responses when backend response caching is not needed.
#[derive(Debug, Clone, Default)]
pub struct ConditionalRequestLayer {
    weak_etags: bool,
}

impl ConditionalRequestLayer {
    /// Creates a [`ConditionalRequestLayer`] emitting strong `ETag`s.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures weak vs. strong `ETag` generation.
    pub fn with_weak_etags(mut self, weak: bool) -> Self {
        self.weak_etags = weak;
        self
    }
}

impl<S> Layer<S> for ConditionalRequestLayer {
    type Service = ConditionalRequestService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConditionalRequestService {
            inner,
            weak_etags: self.weak_etags,
        }
    }
}

/// Tower service produced by [`ConditionalRequestLayer`].
#[derive(Clone)]
pub struct ConditionalRequestService<S> {
    inner: S,
    weak_etags: bool,
}

impl<S> Service<Request<Body>> for ConditionalRequestService<S>
where
    S: Service<Request<Body>> + Clone + Send + 'static,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let weak_etags = self.weak_etags;

        Box::pin(async move {
            let method = req.method().clone();
            if method != Method::GET && method != Method::HEAD {
                return Ok(inner.call(req).await?.into_response());
            }

            let if_none_match = header_str(req.headers().get(header::IF_NONE_MATCH));
            let res = inner.call(req).await?.into_response();
            if res.status() != StatusCode::OK {
                return Ok(res);
            }

            let (mut parts, body) = res.into_parts();
            let Ok(bytes) = to_bytes(body, MAX_CACHEABLE_BODY_BYTES).await else {
                return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
            };

            let etag = parts
                .headers
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string)
                .unwrap_or_else(|| compute_etag(&bytes, weak_etags));

            if let Some(ref inm) = if_none_match
                && etag_matches_weak(inm, &etag)
            {
                let mut res = StatusCode::NOT_MODIFIED.into_response();
                if let Ok(val) = HeaderValue::from_str(&etag) {
                    res.headers_mut().insert(header::ETAG, val);
                }
                return Ok(res);
            }

            if let Ok(val) = HeaderValue::from_str(&etag) {
                parts.headers.insert(header::ETAG, val);
            }
            let out_body = if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(bytes)
            };
            Ok(Response::from_parts(parts, out_body))
        })
    }
}

fn derive_cache_key(req: &Request<Body>, vary_headers: &[HeaderName]) -> String {
    let pq = req
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or_else(|| req.uri().path());
    let mut key = format!("rume:http_cache:{pq}");
    for name in vary_headers {
        let val = req
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        key.push('|');
        key.push_str(name.as_str());
        key.push('=');
        key.push_str(val);
    }
    key
}

fn compute_etag(body: &[u8], weak: bool) -> String {
    let digest = Sha256::digest(body);
    let hex_digest = hex::encode(digest);
    if weak {
        format!("W/\"{hex_digest}\"")
    } else {
        format!("\"{hex_digest}\"")
    }
}

fn header_str(val: Option<&HeaderValue>) -> Option<String> {
    val.and_then(|v| v.to_str().ok()).map(ToString::to_string)
}

fn etag_matches_weak(header_val: &str, current_etag: &str) -> bool {
    let current_norm = current_etag.trim().trim_start_matches("W/");
    header_val
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate.trim_start_matches("W/") == current_norm)
}

fn etag_matches_strong(header_val: &str, current_etag: &str) -> bool {
    if current_etag.trim().starts_with("W/") {
        return false;
    }
    header_val.split(',').map(str::trim).any(|candidate| {
        if candidate == "*" {
            return true;
        }
        !candidate.starts_with("W/") && candidate == current_etag.trim()
    })
}

fn apply_cache_headers(headers: &mut axum::http::HeaderMap, config: &HttpCacheConfig) {
    if let Some(ref cc) = config.cache_control
        && !headers.contains_key(header::CACHE_CONTROL)
        && let Ok(val) = HeaderValue::from_str(cc)
    {
        headers.insert(header::CACHE_CONTROL, val);
    }
    if !config.vary_headers.is_empty() && !headers.contains_key(header::VARY) {
        let vary_val = config
            .vary_headers
            .iter()
            .map(HeaderName::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if let Ok(val) = HeaderValue::from_str(&vary_val) {
            headers.insert(header::VARY, val);
        }
    }
}

fn not_modified_response(etag: &str, config: &HttpCacheConfig) -> Response {
    let mut res = StatusCode::NOT_MODIFIED.into_response();
    if let Ok(val) = HeaderValue::from_str(etag) {
        res.headers_mut().insert(header::ETAG, val);
    }
    apply_cache_headers(res.headers_mut(), config);
    res
}

fn response_from_cached(
    cached: CachedHttpResponse,
    head_only: bool,
    config: &HttpCacheConfig,
) -> Response {
    let status = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(cached.body)
    };
    let mut res = Response::new(body);
    *res.status_mut() = status;
    for (k, v) in cached.headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_bytes(&v),
        ) {
            res.headers_mut().insert(name, val);
        }
    }
    if let Ok(val) = HeaderValue::from_str(&cached.etag) {
        res.headers_mut().insert(header::ETAG, val);
    }
    apply_cache_headers(res.headers_mut(), config);
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode, header};
    use axum::routing::{get, put};
    use rume_data::cache::InMemoryCacheBackend;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;

    #[tokio::test]
    async fn http_cache_returns_304_on_matching_if_none_match_and_caches_in_backend() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);

        let app = Router::new()
            .route(
                "/items",
                get(move || {
                    let hits = Arc::clone(&hits_clone);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        "hello-cache"
                    }
                }),
            )
            .layer(HttpCacheLayer::new(
                backend,
                HttpCacheConfig::new().with_ttl(Duration::from_secs(60)),
            ));

        // First request: cache miss, inner handler called once.
        let res1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res1.status(), StatusCode::OK);
        let etag = res1
            .headers()
            .get(header::ETAG)
            .expect("missing ETag")
            .to_str()
            .unwrap()
            .to_string();
        assert!(res1.headers().get(header::CACHE_CONTROL).is_some());
        let body1 = to_bytes(res1.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body1[..], b"hello-cache");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Second request: served from cache, inner handler NOT called again.
        let res2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::OK);
        assert_eq!(res2.headers().get(header::ETAG).unwrap(), etag.as_str());
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Third request with If-None-Match: 304 Not Modified.
        let res3 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/items")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res3.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn conditional_if_match_returns_412_on_stale_etag() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let app = Router::new()
            .route("/resource/1", get(|| async { "v1" }))
            .route("/resource/1", put(|| async { "updated" }))
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        // Prime cache with GET.
        let get_res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/resource/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_res.status(), StatusCode::OK);
        let current_etag = get_res
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Stale If-Match -> 412 Precondition Failed.
        let stale_put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/resource/1")
                    .header(header::IF_MATCH, "\"stale-etag\"")
                    .body(Body::from("new"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale_put.status(), StatusCode::PRECONDITION_FAILED);

        // Matching If-Match -> 200 OK.
        let ok_put = app
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/resource/1")
                    .header(header::IF_MATCH, current_etag)
                    .body(Body::from("new"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_put.status(), StatusCode::OK);
    }
}
