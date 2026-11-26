//! HTTP response caching and conditional request middleware for Rume.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
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
    /// Whether requests carrying `Authorization` or `Cookie` may read/write the cache.
    pub cache_authenticated: bool,
}

impl Default for HttpCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            cache_control: Some("public, max-age=60".to_string()),
            weak_etags: false,
            vary_headers: Vec::new(),
            cache_authenticated: false,
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

    /// Opts in to caching requests carrying `Authorization` or `Cookie` headers.
    pub fn with_cache_authenticated(mut self, allow: bool) -> Self {
        self.cache_authenticated = allow;
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
                let has_auth_headers = req.headers().contains_key(header::AUTHORIZATION)
                    || req.headers().contains_key(header::COOKIE);
                if !config.cache_authenticated && has_auth_headers {
                    return Ok(inner.call(req).await?.into_response());
                }

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
                if res.status() != StatusCode::OK || !is_cacheable_response(res.headers()) {
                    return Ok(res);
                }

                let content_length_too_large = res
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<usize>().ok())
                    .is_some_and(|len| len > MAX_CACHEABLE_BODY_BYTES);
                if content_length_too_large {
                    return Ok(res);
                }

                let (mut parts, body) = res.into_parts();
                let mut stream = body.into_data_stream();
                let mut buf = Vec::new();
                let mut overflow_chunk: Option<axum::body::Bytes> = None;
                let mut stream_err = false;
                while let Some(next) = stream.next().await {
                    match next {
                        Ok(chunk) => {
                            if buf.len().saturating_add(chunk.len()) > MAX_CACHEABLE_BODY_BYTES {
                                overflow_chunk = Some(chunk);
                                break;
                            }
                            buf.extend_from_slice(&chunk);
                        }
                        Err(_) => {
                            stream_err = true;
                            break;
                        }
                    }
                }
                if overflow_chunk.is_some() || stream_err {
                    let out_body = if method == Method::HEAD {
                        Body::empty()
                    } else {
                        let prefix = futures_util::stream::iter(
                            (!buf.is_empty())
                                .then(|| Ok(axum::body::Bytes::from(buf)))
                                .into_iter()
                                .chain(overflow_chunk.map(Ok)),
                        );
                        Body::from_stream(prefix.chain(stream))
                    };
                    return Ok(Response::from_parts(parts, out_body));
                }
                let bytes = axum::body::Bytes::from(buf);

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

                // Why: HEAD responses carry an empty body, so caching a HEAD miss
                // would poison the shared GET cache entry with an empty body and ETag.
                if method == Method::GET {
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
                if let Some(if_match) = header_str(req.headers().get(header::IF_MATCH))
                    && let Ok(Some(raw)) = backend.get(&cache_key).await
                    && let Ok(cached) = serde_json::from_slice::<CachedHttpResponse>(&raw)
                    && !etag_matches_strong(&if_match, &cached.etag)
                {
                    return Ok(StatusCode::PRECONDITION_FAILED.into_response());
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
            let Ok(bytes) = axum::body::to_bytes(body, MAX_CACHEABLE_BODY_BYTES).await else {
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

fn is_cacheable_response(headers: &axum::http::HeaderMap) -> bool {
    if headers.contains_key(header::SET_COOKIE) {
        return false;
    }
    for val in headers.get_all(header::VARY) {
        if let Ok(s) = val.to_str()
            && s.split(',').any(|token| token.trim() == "*")
        {
            return false;
        }
    }
    for val in headers.get_all(header::CACHE_CONTROL) {
        if let Ok(s) = val.to_str() {
            for directive in s.split(',') {
                let name = directive.split('=').next().unwrap_or("").trim();
                if name.eq_ignore_ascii_case("private")
                    || name.eq_ignore_ascii_case("no-store")
                    || name.eq_ignore_ascii_case("no-cache")
                {
                    return false;
                }
            }
        }
    }
    true
}

fn push_framed(buf: &mut String, value: &str) {
    use std::fmt::Write as _;
    let _ = write!(buf, "{}:{value}|", value.len());
}

fn derive_cache_key(req: &Request<Body>, vary_headers: &[HeaderName]) -> String {
    let pq = req
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or_else(|| req.uri().path());
    let tenant_id = req
        .extensions()
        .get::<rume_security::tenant::Tenant>()
        .map(|t| t.id.as_str())
        .or_else(|| {
            req.extensions()
                .get::<rume_security::tenant::TenantContext>()
                .map(|tc| tc.id.as_str())
        });

    let mut key = String::from("rume:http_cache:");
    match tenant_id {
        Some(tid) => {
            key.push_str("1:");
            push_framed(&mut key, tid);
        }
        None => key.push_str("0:|"),
    }
    push_framed(&mut key, pq);
    for name in vary_headers {
        push_framed(&mut key, name.as_str());
        let val = req
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        push_framed(&mut key, val);
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
            res.headers_mut().append(name, val);
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

    #[tokio::test]
    async fn head_miss_does_not_poison_subsequent_get_cache() {
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

        // 1. Cold HEAD request: returns 200 with empty body, must NOT write to cache.
        let head_miss = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head_miss.status(), StatusCode::OK);
        let head_miss_body = to_bytes(head_miss.into_body(), usize::MAX).await.unwrap();
        assert!(head_miss_body.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // 2. Subsequent GET request: cache miss, populates cache with full body and correct ETag.
        let get_res = app
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
        assert_eq!(get_res.status(), StatusCode::OK);
        let expected_etag = compute_etag(b"hello-cache", false);
        assert_eq!(
            get_res.headers().get(header::ETAG).unwrap(),
            expected_etag.as_str()
        );
        let get_body = to_bytes(get_res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&get_body[..], b"hello-cache");
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // 3. Subsequent HEAD request (cache hit): serves empty body with the GET ETag.
        let head_hit = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head_hit.status(), StatusCode::OK);
        assert_eq!(
            head_hit.headers().get(header::ETAG).unwrap(),
            expected_etag.as_str()
        );
        let head_hit_body = to_bytes(head_hit.into_body(), usize::MAX).await.unwrap();
        assert!(head_hit_body.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn authenticated_and_private_responses_are_never_replayed_to_other_callers() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let auth_hits = Arc::new(AtomicUsize::new(0));
        let auth_hits_clone = Arc::clone(&auth_hits);
        let cookie_hits = Arc::new(AtomicUsize::new(0));
        let cookie_hits_clone = Arc::clone(&cookie_hits);
        let private_hits = Arc::new(AtomicUsize::new(0));
        let private_hits_clone = Arc::clone(&private_hits);

        let app = Router::new()
            .route(
                "/auth-secret",
                get(move |req: Request<Body>| {
                    let hits = Arc::clone(&auth_hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        let auth = req
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("none")
                            .to_owned();
                        format!("secret-{n}-{auth}")
                    }
                }),
            )
            .route(
                "/login",
                get(move || {
                    let hits = Arc::clone(&cookie_hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        (
                            [(header::SET_COOKIE, format!("sid=user-{n}; HttpOnly"))],
                            format!("logged-in-{n}"),
                        )
                    }
                }),
            )
            .route(
                "/private",
                get(move || {
                    let hits = Arc::clone(&private_hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        (
                            [(header::CACHE_CONTROL, "private, max-age=60")],
                            format!("private-{n}"),
                        )
                    }
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        // 1. Request with Authorization must bypass cache read/write.
        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/auth-secret")
                    .header(header::AUTHORIZATION, "Bearer alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r1.into_body(), usize::MAX).await.unwrap(),
            b"secret-1-Bearer alice".as_slice()
        );

        let r2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/auth-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r2.into_body(), usize::MAX).await.unwrap(),
            b"secret-2-none".as_slice()
        );
        assert_eq!(auth_hits.load(Ordering::SeqCst), 2);

        // 2. Response with Set-Cookie must not be cached or replayed.
        let c1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            c1.headers().get(header::SET_COOKIE).unwrap(),
            "sid=user-1; HttpOnly"
        );
        let c2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            c2.headers().get(header::SET_COOKIE).unwrap(),
            "sid=user-2; HttpOnly"
        );
        assert_eq!(cookie_hits.load(Ordering::SeqCst), 2);

        // 3. Response with Cache-Control: private must not be cached or replayed.
        let p1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/private")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(p1.into_body(), usize::MAX).await.unwrap(),
            b"private-1".as_slice()
        );
        let p2 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/private")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(p2.into_body(), usize::MAX).await.unwrap(),
            b"private-2".as_slice()
        );
        assert_eq!(private_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn large_body_exceeding_max_cacheable_bytes_passes_through_without_500() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let large_size = MAX_CACHEABLE_BODY_BYTES + 1024 * 1024; // 9 MiB

        let app = Router::new()
            .route(
                "/large-known",
                get(move || async move { vec![b'x'; large_size] }),
            )
            .route(
                "/large-stream",
                get(move || async move {
                    let chunk = vec![b'y'; 1024 * 1024];
                    let stream = futures_util::stream::iter(
                        (0..9).map(move |_| Ok::<_, std::convert::Infallible>(chunk.clone())),
                    );
                    Body::from_stream(stream)
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let res_known = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/large-known")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_known.status(), StatusCode::OK);
        let body_known = to_bytes(res_known.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body_known.len(), large_size);

        let res_stream = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/large-stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_stream.status(), StatusCode::OK);
        let body_stream = to_bytes(res_stream.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body_stream.len(), 9 * 1024 * 1024);
    }

    #[tokio::test]
    async fn put_with_if_match_wildcard_on_cold_cache_reaches_inner_handler() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let put_hits = Arc::new(AtomicUsize::new(0));
        let put_hits_clone = Arc::clone(&put_hits);

        let app = Router::new()
            .route(
                "/resource/cold",
                put(move || {
                    let hits = Arc::clone(&put_hits_clone);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        "updated-cold"
                    }
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let res = app
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/resource/cold")
                    .header(header::IF_MATCH, "*")
                    .body(Body::from("payload"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(put_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tenant_isolation_and_multi_value_headers_are_preserved() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);

        let app = Router::new()
            .route(
                "/tenant-items",
                get(move |req: Request<Body>| {
                    let hits = Arc::clone(&hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        let tenant = req
                            .extensions()
                            .get::<rume_security::tenant::Tenant>()
                            .map(|t| t.id.clone())
                            .unwrap_or_else(|| "none".to_string());
                        let mut res = Response::new(Body::from(format!("{tenant}-{n}")));
                        res.headers_mut().append(
                            HeaderName::from_static("x-multi"),
                            HeaderValue::from_static("one"),
                        );
                        res.headers_mut().append(
                            HeaderName::from_static("x-multi"),
                            HeaderValue::from_static("two"),
                        );
                        res
                    }
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let mut req_a = Request::builder()
            .method(Method::GET)
            .uri("/tenant-items")
            .body(Body::empty())
            .unwrap();
        req_a
            .extensions_mut()
            .insert(rume_security::tenant::Tenant::new("tenant-a", "a"));
        let res_a = app.clone().oneshot(req_a).await.unwrap();
        assert_eq!(
            to_bytes(res_a.into_body(), usize::MAX).await.unwrap(),
            b"tenant-a-1".as_slice()
        );

        let mut req_b = Request::builder()
            .method(Method::GET)
            .uri("/tenant-items")
            .body(Body::empty())
            .unwrap();
        req_b
            .extensions_mut()
            .insert(rume_security::tenant::Tenant::new("tenant-b", "b"));
        let res_b = app.clone().oneshot(req_b).await.unwrap();
        assert_eq!(
            to_bytes(res_b.into_body(), usize::MAX).await.unwrap(),
            b"tenant-b-2".as_slice()
        );

        // Cached hit for tenant-a preserves multi-value headers.
        let mut req_a2 = Request::builder()
            .method(Method::GET)
            .uri("/tenant-items")
            .body(Body::empty())
            .unwrap();
        req_a2
            .extensions_mut()
            .insert(rume_security::tenant::Tenant::new("tenant-a", "a"));
        let res_a2 = app.oneshot(req_a2).await.unwrap();
        let multi: Vec<&str> = res_a2
            .headers()
            .get_all("x-multi")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(multi, vec!["one", "two"]);
        assert_eq!(
            to_bytes(res_a2.into_body(), usize::MAX).await.unwrap(),
            b"tenant-a-1".as_slice()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
