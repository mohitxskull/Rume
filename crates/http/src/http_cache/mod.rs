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
            let authority = match extract_request_authority(&req) {
                RequestAuthority::Present(auth) => Some(auth),
                RequestAuthority::Absent => None,
                // Why: multiple or malformed `Host` headers create proxy/origin ambiguity
                // (`RC-01`); fail closed by bypassing cache read and write completely.
                RequestAuthority::Ambiguous => {
                    return Ok(inner.call(req).await?.into_response());
                }
            };
            let gen_key = derive_generation_key(&req, authority.as_deref());
            let generation = load_generation(backend.as_ref(), &gen_key).await;
            let cache_key = derive_cache_key(&req, authority.as_deref(), &config, generation);

            if method == Method::GET || method == Method::HEAD {
                let has_auth_headers = req.headers().contains_key(header::AUTHORIZATION)
                    || req.headers().contains_key(header::COOKIE);
                if !config.cache_authenticated && has_auth_headers {
                    return Ok(inner.call(req).await?.into_response());
                }

                let req_cc = parse_request_cache_control(req.headers());
                let if_none_match = joined_header_values(req.headers(), &header::IF_NONE_MATCH);
                if !req_cc.skip_read
                    && let Ok(Some(raw)) = backend.get(&cache_key).await
                    && let Ok(cached) = serde_json::from_slice::<CachedHttpResponse>(&raw)
                {
                    if let Some(ref inm) = if_none_match
                        && etag_matches_weak(inm, &cached.etag)
                    {
                        return Ok(not_modified_response(
                            &cached.etag,
                            &config,
                            has_auth_headers,
                        ));
                    }
                    return Ok(response_from_cached(
                        cached,
                        method == Method::HEAD,
                        &config,
                        has_auth_headers,
                    ));
                }

                let res = inner.call(req).await?.into_response();
                if res.status() != StatusCode::OK
                    || !is_cacheable_response(res.headers(), &config.vary_headers)
                {
                    return Ok(res);
                }

                let effective_ttl = response_cache_ttl(res.headers(), config.ttl);
                let (mut parts, body) = res.into_parts();
                let bytes = match buffer_or_passthrough_body(
                    &parts.headers,
                    body,
                    MAX_CACHEABLE_BODY_BYTES,
                    method == Method::HEAD,
                )
                .await
                {
                    BufferedBody::Complete(bytes) => bytes,
                    BufferedBody::Passthrough(out_body) => {
                        return Ok(Response::from_parts(parts, out_body));
                    }
                    BufferedBody::StreamError => {
                        return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    }
                };

                // Why: Per RFC 9110 §9.3.2, a HEAD response must emit the same ETag as GET.
                // On a cold HEAD miss without a handler-provided ETag, the body is empty, so
                // synthesizing an ETag from `b""` would emit a bogus validator that disagrees
                // with GET. Only compute an ETag from buffered bytes for GET responses.
                let etag = parts
                    .headers
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(ToString::to_string)
                    .or_else(|| {
                        (method != Method::HEAD).then(|| compute_etag(&bytes, config.weak_etags))
                    });

                if let Some(ref etag_val) = etag
                    && let Ok(val) = HeaderValue::from_str(etag_val)
                {
                    parts.headers.insert(header::ETAG, val);
                }
                apply_cache_headers(&mut parts.headers, &config, has_auth_headers);

                // Why: HEAD responses carry an empty body, so caching a HEAD miss
                // would poison the shared GET cache entry with an empty body and ETag.
                if method == Method::GET
                    && !req_cc.skip_write
                    && let Some(ref etag_val) = etag
                {
                    let serialized_headers = parts
                        .headers
                        .iter()
                        .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
                        .collect();

                    let cached = CachedHttpResponse {
                        status: parts.status.as_u16(),
                        headers: serialized_headers,
                        body: bytes.to_vec(),
                        etag: etag_val.clone(),
                    };
                    if let Ok(encoded) = serde_json::to_vec(&cached) {
                        let _ = backend.set(&cache_key, encoded, Some(effective_ttl)).await;
                    }
                }

                if let Some(ref inm) = if_none_match
                    && let Some(ref etag_val) = etag
                    && etag_matches_weak(inm, etag_val)
                {
                    return Ok(not_modified_response(etag_val, &config, has_auth_headers));
                }

                let out_body = if method == Method::HEAD {
                    Body::empty()
                } else {
                    Body::from(bytes)
                };
                return Ok(Response::from_parts(parts, out_body));
            }

            if !method.is_safe() {
                // Why: when `config.vary_headers` is non-empty, `cache_key` depends on request
                // headers that a mutating request may omit or send differently, so rejecting
                // `If-Match` against a single cached variant risks false-positive 412s or
                // checking the wrong representation. Delegate `If-Match` enforcement to the
                // inner handler which holds authoritative ETag state (`RC-02`).
                if config.vary_headers.is_empty()
                    && matches!(method, Method::PUT | Method::PATCH | Method::DELETE)
                    && let Some(if_match) = joined_header_values(req.headers(), &header::IF_MATCH)
                    && let Ok(Some(raw)) = backend.get(&cache_key).await
                    && let Ok(cached) = serde_json::from_slice::<CachedHttpResponse>(&raw)
                    && !etag_matches_strong(&if_match, &cached.etag)
                {
                    return Ok(StatusCode::PRECONDITION_FAILED.into_response());
                }

                let res = inner.call(req).await?.into_response();
                if res.status().is_success() {
                    let _ = backend.delete(&cache_key).await;
                    bump_generation(backend.as_ref(), &gen_key, config.ttl).await;
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

            let if_none_match = joined_header_values(req.headers(), &header::IF_NONE_MATCH);
            let res = inner.call(req).await?.into_response();
            if res.status() != StatusCode::OK {
                return Ok(res);
            }

            let (mut parts, body) = res.into_parts();
            let bytes = match buffer_or_passthrough_body(
                &parts.headers,
                body,
                MAX_CACHEABLE_BODY_BYTES,
                method == Method::HEAD,
            )
            .await
            {
                BufferedBody::Complete(bytes) => bytes,
                BufferedBody::Passthrough(out_body) => {
                    return Ok(Response::from_parts(parts, out_body));
                }
                BufferedBody::StreamError => {
                    return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            };

            let etag = parts
                .headers
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string)
                .or_else(|| (method != Method::HEAD).then(|| compute_etag(&bytes, weak_etags)));

            if let Some(ref inm) = if_none_match
                && let Some(ref etag_val) = etag
                && etag_matches_weak(inm, etag_val)
            {
                let mut res = StatusCode::NOT_MODIFIED.into_response();
                if let Ok(val) = HeaderValue::from_str(etag_val) {
                    res.headers_mut().insert(header::ETAG, val);
                }
                return Ok(res);
            }

            if let Some(ref etag_val) = etag
                && let Ok(val) = HeaderValue::from_str(etag_val)
            {
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

enum BufferedBody {
    /// Entire body fit within `max_bytes`.
    Complete(axum::body::Bytes),
    /// Body exceeded `max_bytes` (via `Content-Length` or while streaming) and must be
    /// passed through as a streaming body without caching or ETag hashing.
    Passthrough(Body),
    /// Upstream body stream errored while buffering before response headers were sent.
    StreamError,
}

/// Buffers a response body up to `max_bytes`, switching seamlessly to streaming pass-through
/// if the body exceeds `max_bytes`.
///
/// Why: never turn a valid >`max_bytes` response into `500`, and never swallow a mid-stream
/// `Err(e)` into a truncated `200 OK`: errors before pass-through yield `StreamError` (`500`),
/// while errors after pass-through begins propagate inside `Body::from_stream`.
async fn buffer_or_passthrough_body(
    headers: &axum::http::HeaderMap,
    body: Body,
    max_bytes: usize,
    head_only: bool,
) -> BufferedBody {
    let content_length_too_large = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .is_some_and(|len| len > max_bytes);
    if content_length_too_large {
        return BufferedBody::Passthrough(if head_only { Body::empty() } else { body });
    }

    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    while let Some(next) = stream.next().await {
        match next {
            Ok(chunk) => {
                if buf.len().saturating_add(chunk.len()) > max_bytes {
                    let out_body = if head_only {
                        Body::empty()
                    } else {
                        let prefix = futures_util::stream::iter(
                            (!buf.is_empty())
                                .then(|| Ok(axum::body::Bytes::from(buf)))
                                .into_iter()
                                .chain(std::iter::once(Ok(chunk))),
                        );
                        Body::from_stream(prefix.chain(stream))
                    };
                    return BufferedBody::Passthrough(out_body);
                }
                buf.extend_from_slice(&chunk);
            }
            Err(_) => {
                return BufferedBody::StreamError;
            }
        }
    }

    BufferedBody::Complete(axum::body::Bytes::from(buf))
}

struct RequestCacheDirectives {
    skip_read: bool,
    skip_write: bool,
}

fn parse_directive_delta_seconds(raw_value: Option<&str>) -> Option<u64> {
    let v = raw_value?.trim().trim_matches('"');
    v.parse::<u64>().ok()
}

fn parse_request_cache_control(headers: &axum::http::HeaderMap) -> RequestCacheDirectives {
    let mut directives = RequestCacheDirectives {
        skip_read: false,
        skip_write: false,
    };
    for val in headers.get_all(header::CACHE_CONTROL) {
        let Ok(s) = val.to_str() else {
            return RequestCacheDirectives {
                skip_read: true,
                skip_write: true,
            };
        };
        for directive in s.split(',') {
            let mut parts = directive.splitn(2, '=');
            let name = parts.next().unwrap_or("").trim();
            let value = parts.next();
            if name.eq_ignore_ascii_case("no-store") {
                directives.skip_read = true;
                directives.skip_write = true;
            } else if name.eq_ignore_ascii_case("no-cache")
                || (name.eq_ignore_ascii_case("max-age")
                    && parse_directive_delta_seconds(value).is_none_or(|secs| secs == 0))
            {
                directives.skip_read = true;
            }
        }
    }
    directives
}

fn response_cache_ttl(headers: &axum::http::HeaderMap, config_ttl: Duration) -> Duration {
    let mut s_maxage: Option<u64> = None;
    let mut max_age: Option<u64> = None;
    for val in headers.get_all(header::CACHE_CONTROL) {
        if let Ok(s) = val.to_str() {
            for directive in s.split(',') {
                let mut parts = directive.splitn(2, '=');
                let name = parts.next().unwrap_or("").trim();
                let value = parts.next();
                if name.eq_ignore_ascii_case("s-maxage")
                    && let Some(secs) = parse_directive_delta_seconds(value)
                {
                    s_maxage = Some(s_maxage.map_or(secs, |cur| cur.min(secs)));
                } else if name.eq_ignore_ascii_case("max-age")
                    && let Some(secs) = parse_directive_delta_seconds(value)
                {
                    max_age = Some(max_age.map_or(secs, |cur| cur.min(secs)));
                }
            }
        }
    }
    match s_maxage.or(max_age) {
        Some(secs) => config_ttl.min(Duration::from_secs(secs)),
        None => config_ttl,
    }
}

enum RequestAuthority {
    Present(String),
    Absent,
    Ambiguous,
}

fn extract_request_authority(req: &Request<Body>) -> RequestAuthority {
    let mut hosts = req.headers().get_all(header::HOST).iter();
    let header_host = match (hosts.next(), hosts.next()) {
        (Some(_), Some(_)) => return RequestAuthority::Ambiguous,
        (Some(val), None) => {
            let Ok(s) = val.to_str() else {
                return RequestAuthority::Ambiguous;
            };
            Some(s.trim().to_ascii_lowercase())
        }
        (None, _) => None,
    };
    let uri_authority = req
        .uri()
        .authority()
        .map(|a| a.as_str().to_ascii_lowercase());
    match (header_host, uri_authority) {
        (Some(h), Some(u)) if h != u => RequestAuthority::Ambiguous,
        (Some(h), _) => RequestAuthority::Present(h),
        (None, Some(u)) => RequestAuthority::Present(u),
        (None, None) => RequestAuthority::Absent,
    }
}

fn is_cacheable_response(headers: &axum::http::HeaderMap, vary_headers: &[HeaderName]) -> bool {
    if headers.contains_key(header::SET_COOKIE) {
        return false;
    }
    for val in headers.get_all(header::VARY) {
        let Ok(s) = val.to_str() else {
            return false;
        };
        for token in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            if token == "*"
                || !vary_headers
                    .iter()
                    .any(|configured| configured.as_str().eq_ignore_ascii_case(token))
            {
                return false;
            }
        }
    }
    for val in headers.get_all(header::CACHE_CONTROL) {
        // Why: fail closed on non-UTF-8 `Cache-Control` headers, matching `VARY` (`RC-03`).
        let Ok(s) = val.to_str() else {
            return false;
        };
        for directive in s.split(',') {
            let mut parts = directive.splitn(2, '=');
            let name = parts.next().unwrap_or("").trim();
            let value = parts.next();
            if name.eq_ignore_ascii_case("private")
                || name.eq_ignore_ascii_case("no-store")
                || name.eq_ignore_ascii_case("no-cache")
            {
                return false;
            }
            if (name.eq_ignore_ascii_case("max-age") || name.eq_ignore_ascii_case("s-maxage"))
                && parse_directive_delta_seconds(value).is_none_or(|secs| secs == 0)
            {
                return false;
            }
        }
    }
    true
}

fn push_framed(buf: &mut String, value: &str) {
    use std::fmt::Write as _;
    let _ = write!(buf, "{}:{value}|", value.len());
}

fn hash_header_values(headers: &axum::http::HeaderMap, name: &HeaderName) -> Option<String> {
    let mut iter = headers.get_all(name).iter().peekable();
    iter.peek()?;
    let mut hasher = Sha256::new();
    for val in iter {
        let bytes = val.as_bytes();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Some(hex::encode(hasher.finalize()))
}

fn extract_tenant_id(req: &Request<Body>) -> Option<&str> {
    req.extensions()
        .get::<rume_security::tenant::Tenant>()
        .map(|t| t.id.as_str())
        .or_else(|| {
            req.extensions()
                .get::<rume_security::tenant::TenantContext>()
                .map(|tc| tc.id.as_str())
        })
}

fn derive_generation_key(req: &Request<Body>, authority: Option<&str>) -> String {
    let mut key = String::from("rume:http_cache_gen:");
    match authority {
        Some(auth) => {
            key.push_str("h1:");
            push_framed(&mut key, auth);
        }
        None => key.push_str("h0:|"),
    }
    match extract_tenant_id(req) {
        Some(tid) => {
            key.push_str("1:");
            push_framed(&mut key, tid);
        }
        None => key.push_str("0:|"),
    }
    push_framed(&mut key, req.uri().path());
    key
}

async fn load_generation(backend: &dyn CacheBackend, gen_key: &str) -> u128 {
    backend
        .get(gen_key)
        .await
        .ok()
        .flatten()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes.as_slice()).ok())
        .map(u128::from_be_bytes)
        .unwrap_or(0)
}

async fn bump_generation(backend: &dyn CacheBackend, gen_key: &str, ttl: Duration) {
    // Why: write a random 128-bit UUID v4 generation token directly without reading
    // any prior counter. A sequential `u64` RMW counter resets to `1` whenever `gen_key`
    // expires or is evicted, which can collide with a still-live entry cached under
    // generation `1` and resurrect stale data (`RC-03`).
    let token = uuid::Uuid::new_v4().as_u128();
    let _ = backend
        .set(
            gen_key,
            token.to_be_bytes().to_vec(),
            Some(ttl.saturating_mul(2)),
        )
        .await;
}

fn derive_cache_key(
    req: &Request<Body>,
    authority: Option<&str>,
    config: &HttpCacheConfig,
    generation: u128,
) -> String {
    let pq = req
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or_else(|| req.uri().path());
    let tenant_id = extract_tenant_id(req);

    let mut key = String::from("rume:http_cache:");
    key.push_str(&generation.to_string());
    key.push(':');
    match authority {
        Some(auth) => {
            key.push_str("h1:");
            push_framed(&mut key, auth);
        }
        None => key.push_str("h0:|"),
    }
    match tenant_id {
        Some(tid) => {
            key.push_str("1:");
            push_framed(&mut key, tid);
        }
        None => key.push_str("0:|"),
    }
    push_framed(&mut key, pq);
    for name in &config.vary_headers {
        push_framed(&mut key, name.as_str());
        match hash_header_values(req.headers(), name) {
            Some(digest) => {
                key.push_str("1:");
                push_framed(&mut key, &digest);
            }
            None => key.push_str("0:|"),
        }
    }
    if config.cache_authenticated {
        match hash_header_values(req.headers(), &header::AUTHORIZATION) {
            Some(digest) => {
                key.push_str("a1:");
                push_framed(&mut key, &digest);
            }
            None => key.push_str("a0:|"),
        }
        match hash_header_values(req.headers(), &header::COOKIE) {
            Some(digest) => {
                key.push_str("c1:");
                push_framed(&mut key, &digest);
            }
            None => key.push_str("c0:|"),
        }
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

fn joined_header_values(headers: &axum::http::HeaderMap, name: &HeaderName) -> Option<String> {
    let mut joined = String::new();
    let mut found = false;
    for val in headers.get_all(name) {
        if let Ok(s) = val.to_str() {
            if found {
                joined.push_str(", ");
            }
            joined.push_str(s);
            found = true;
        }
    }
    found.then_some(joined)
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

fn rewrite_cache_control_private(cc: &str) -> String {
    let mut directives: Vec<&str> = Vec::new();
    let mut has_private = false;
    for raw in cc.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = trimmed.split('=').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("public") {
            if !has_private {
                directives.push("private");
                has_private = true;
            }
        } else {
            if name.eq_ignore_ascii_case("private") {
                has_private = true;
            }
            directives.push(trimmed);
        }
    }
    if !has_private {
        directives.insert(0, "private");
    }
    directives.join(", ")
}

fn apply_cache_headers(
    headers: &mut axum::http::HeaderMap,
    config: &HttpCacheConfig,
    is_authenticated: bool,
) {
    if is_authenticated {
        if let Some(existing) = joined_header_values(headers, &header::CACHE_CONTROL)
            .map(|s| rewrite_cache_control_private(&s))
            .and_then(|s| HeaderValue::from_str(&s).ok())
        {
            headers.insert(header::CACHE_CONTROL, existing);
        } else if let Some(ref cc) = config.cache_control
            && let Ok(val) = HeaderValue::from_str(&rewrite_cache_control_private(cc))
        {
            headers.insert(header::CACHE_CONTROL, val);
        }
    } else if let Some(ref cc) = config.cache_control
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

fn not_modified_response(etag: &str, config: &HttpCacheConfig, is_authenticated: bool) -> Response {
    let mut res = StatusCode::NOT_MODIFIED.into_response();
    if let Ok(val) = HeaderValue::from_str(etag) {
        res.headers_mut().insert(header::ETAG, val);
    }
    apply_cache_headers(res.headers_mut(), config, is_authenticated);
    res
}

fn response_from_cached(
    cached: CachedHttpResponse,
    head_only: bool,
    config: &HttpCacheConfig,
    is_authenticated: bool,
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
    apply_cache_headers(res.headers_mut(), config, is_authenticated);
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

    #[tokio::test]
    async fn mid_stream_error_is_not_silently_truncated_to_200_ok() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let cache_app = Router::new()
            .route(
                "/boom",
                get(|| async {
                    let stream = futures_util::stream::iter(vec![
                        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from_static(
                            b"partial",
                        )),
                        Err(std::io::Error::other("upstream disconnect")),
                    ]);
                    Body::from_stream(stream)
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let res = cache_app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/boom")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let cond_app = Router::new()
            .route(
                "/boom",
                get(|| async {
                    let stream = futures_util::stream::iter(vec![
                        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from_static(
                            b"partial",
                        )),
                        Err(std::io::Error::other("upstream disconnect")),
                    ]);
                    Body::from_stream(stream)
                }),
            )
            .layer(ConditionalRequestLayer::new());

        let cond_res = cond_app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/boom")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cond_res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn large_9mib_response_through_conditional_request_layer_returns_200() {
        let large_size = MAX_CACHEABLE_BODY_BYTES + 1024 * 1024; // 9 MiB
        let app = Router::new()
            .route(
                "/large-known",
                get(move || async move { vec![b'z'; large_size] }),
            )
            .route(
                "/large-stream",
                get(move || async move {
                    let chunk = vec![b'w'; 1024 * 1024];
                    let stream = futures_util::stream::iter(
                        (0..9).map(move |_| Ok::<_, std::convert::Infallible>(chunk.clone())),
                    );
                    Body::from_stream(stream)
                }),
            )
            .layer(ConditionalRequestLayer::new());

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
    async fn cache_authenticated_isolates_users_and_never_emits_public() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);

        let app = Router::new()
            .route(
                "/me",
                get(move |req: Request<Body>| {
                    let hits = Arc::clone(&hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        let auth = req
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("anon")
                            .to_owned();
                        format!("user={auth},hit={n}")
                    }
                }),
            )
            .layer(HttpCacheLayer::new(
                backend,
                HttpCacheConfig::default().with_cache_authenticated(true),
            ));

        let r_alice1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/me")
                    .header(header::AUTHORIZATION, "Bearer alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_alice1.status(), StatusCode::OK);
        let cc_alice1 = r_alice1
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            !cc_alice1.contains("public"),
            "authenticated response must never emit public Cache-Control: {cc_alice1}"
        );
        assert!(
            cc_alice1.contains("private"),
            "authenticated response should emit private Cache-Control: {cc_alice1}"
        );
        assert_eq!(
            to_bytes(r_alice1.into_body(), usize::MAX).await.unwrap(),
            b"user=Bearer alice,hit=1".as_slice()
        );

        let r_bob = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/me")
                    .header(header::AUTHORIZATION, "Bearer bob")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_bob.status(), StatusCode::OK);
        let cc_bob = r_bob
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(!cc_bob.contains("public"));
        assert_eq!(
            to_bytes(r_bob.into_body(), usize::MAX).await.unwrap(),
            b"user=Bearer bob,hit=2".as_slice()
        );

        // Second request for alice hits alice's isolated cache entry.
        let r_alice2 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/me")
                    .header(header::AUTHORIZATION, "Bearer alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cc_alice2 = r_alice2
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(!cc_alice2.contains("public"));
        assert_eq!(
            to_bytes(r_alice2.into_body(), usize::MAX).await.unwrap(),
            b"user=Bearer alice,hit=1".as_slice()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn response_vary_header_outside_config_vary_headers_bypasses_cache() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);

        let app = Router::new()
            .route(
                "/localized",
                get(move |req: Request<Body>| {
                    let hits = Arc::clone(&hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        let lang = req
                            .headers()
                            .get(header::ACCEPT_LANGUAGE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("en")
                            .to_owned();
                        (
                            [(header::VARY, "accept-language")],
                            format!("lang={lang},hit={n}"),
                        )
                    }
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/localized")
                    .header(header::ACCEPT_LANGUAGE, "en")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r1.into_body(), usize::MAX).await.unwrap(),
            b"lang=en,hit=1".as_slice()
        );

        let r2 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/localized")
                    .header(header::ACCEPT_LANGUAGE, "fr")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r2.into_body(), usize::MAX).await.unwrap(),
            b"lang=fr,hit=2".as_slice()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cold_head_does_not_synthesize_empty_body_etag() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let app = Router::new()
            .route("/doc", get(|| async { "actual-document-body" }))
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let res = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/doc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            res.headers().get(header::ETAG).is_none(),
            "cold HEAD without handler ETag must not synthesize a bogus empty-body ETag: {:?}",
            res.headers().get(header::ETAG)
        );
    }

    #[tokio::test]
    async fn put_without_vary_header_invalidates_all_vary_variants() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let version = Arc::new(AtomicUsize::new(1));
        let v_get = Arc::clone(&version);
        let v_put = Arc::clone(&version);

        let app = Router::new()
            .route(
                "/doc",
                get(move |req: Request<Body>| {
                    let v = Arc::clone(&v_get);
                    async move {
                        let lang = req
                            .headers()
                            .get(header::ACCEPT_LANGUAGE)
                            .and_then(|val| val.to_str().ok())
                            .unwrap_or("default")
                            .to_owned();
                        format!("lang={lang},v={}", v.load(Ordering::SeqCst))
                    }
                })
                .put(move || {
                    let v = Arc::clone(&v_put);
                    async move {
                        v.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .layer(HttpCacheLayer::new(
                backend,
                HttpCacheConfig::default().with_vary_headers(vec![header::ACCEPT_LANGUAGE]),
            ));

        // Populate cache for Accept-Language: en and fr.
        let r_en1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/doc")
                    .header(header::ACCEPT_LANGUAGE, "en")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_en1.into_body(), usize::MAX).await.unwrap(),
            b"lang=en,v=1".as_slice()
        );

        let r_fr1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/doc")
                    .header(header::ACCEPT_LANGUAGE, "fr")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_fr1.into_body(), usize::MAX).await.unwrap(),
            b"lang=fr,v=1".as_slice()
        );

        // PUT /doc WITHOUT Accept-Language header must invalidate BOTH en and fr variants.
        let r_put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/doc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_put.status(), StatusCode::NO_CONTENT);

        let r_en2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/doc")
                    .header(header::ACCEPT_LANGUAGE, "en")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_en2.into_body(), usize::MAX).await.unwrap(),
            b"lang=en,v=2".as_slice()
        );

        let r_fr2 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/doc")
                    .header(header::ACCEPT_LANGUAGE, "fr")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_fr2.into_body(), usize::MAX).await.unwrap(),
            b"lang=fr,v=2".as_slice()
        );
    }

    #[tokio::test]
    async fn multi_value_if_none_match_header_lines_match_etag() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let app = Router::new()
            .route("/multi", get(|| async { "payload" }))
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/multi")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = r1
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Send two separate If-None-Match header lines where the matching ETag is on the second line.
        let r2 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/multi")
                    .header(header::IF_NONE_MATCH, "\"unrelated-etag\"")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            r2.status(),
            StatusCode::NOT_MODIFIED,
            "second If-None-Match header line must be evaluated"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_generation_key_never_resurrects_stale_cached_entry() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let version = Arc::new(AtomicUsize::new(1));
        let v_get = Arc::clone(&version);
        let v_put = Arc::clone(&version);

        let app = Router::new()
            .route(
                "/doc",
                get(move || {
                    let v = v_get.load(Ordering::SeqCst);
                    async move { format!("v{v}") }
                })
                .put(move || {
                    let v_put = Arc::clone(&v_put);
                    async move {
                        v_put.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .layer(HttpCacheLayer::new(
                backend,
                HttpCacheConfig::new().with_ttl(Duration::from_secs(10)),
            ));

        // t = 0s: PUT sets v=2 and writes gen_key (expires at t=20s).
        let r_put1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/doc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_put1.status(), StatusCode::NO_CONTENT);

        // t = 15s: GET caches "v2" under gen_1 (entry expires at t=25s).
        tokio::time::advance(Duration::from_secs(15)).await;
        let r_get1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/doc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_get1.into_body(), usize::MAX).await.unwrap(),
            b"v2".as_slice()
        );

        // t = 21s: gen_key expired at t=20s while the cached "v2" entry still lives until t=25s.
        // PUT bumps to v=3 and writes a fresh random UUID v4 generation token (never colliding with gen_1).
        tokio::time::advance(Duration::from_secs(6)).await;
        let r_put2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/doc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_put2.status(), StatusCode::NO_CONTENT);

        // GET at t=21s MUST return "v3" and never resurrect stale "v2".
        let r_get2 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/doc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_get2.into_body(), usize::MAX).await.unwrap(),
            b"v3".as_slice()
        );
    }

    #[tokio::test]
    async fn multi_host_isolation_and_duplicate_host_bypass() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = Arc::clone(&hits);

        let app = Router::new()
            .route(
                "/page",
                get(move |req: Request<Body>| {
                    let hits = Arc::clone(&hits_clone);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        let host = req
                            .headers()
                            .get(header::HOST)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("none")
                            .to_string();
                        format!("{host}:{n}")
                    }
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        // Host A caches its own entry
        let r_a1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/page")
                    .header(header::HOST, "alpha.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_a1.into_body(), usize::MAX).await.unwrap(),
            b"alpha.example.com:1".as_slice()
        );

        // Host B must NOT receive Host A's cached response
        let r_b1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/page")
                    .header(header::HOST, "beta.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_b1.into_body(), usize::MAX).await.unwrap(),
            b"beta.example.com:2".as_slice()
        );

        // Duplicate Host headers must fail closed and bypass cache read/write
        let r_dup = app
            .oneshot(
                Request::builder()
                    .uri("/page")
                    .header(header::HOST, "alpha.example.com")
                    .header(header::HOST, "beta.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_dup.into_body(), usize::MAX).await.unwrap(),
            b"alpha.example.com:3".as_slice()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn post_invalidates_cached_entries() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let version = Arc::new(AtomicUsize::new(1));
        let v_get = Arc::clone(&version);
        let v_post = Arc::clone(&version);

        let app = Router::new()
            .route(
                "/items",
                get(move || {
                    let v = v_get.load(Ordering::SeqCst);
                    async move { format!("v{v}") }
                })
                .post(move || {
                    let v_post = Arc::clone(&v_post);
                    async move {
                        v_post.fetch_add(1, Ordering::SeqCst);
                        StatusCode::CREATED
                    }
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r1.into_body(), usize::MAX).await.unwrap(),
            b"v1".as_slice()
        );

        let r_post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_post.status(), StatusCode::CREATED);

        let r2 = app
            .oneshot(
                Request::builder()
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r2.into_body(), usize::MAX).await.unwrap(),
            b"v2".as_slice()
        );
    }

    #[tokio::test]
    async fn request_and_response_cache_control_directives_enforced() {
        let backend = Arc::new(InMemoryCacheBackend::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_cc = Arc::clone(&hits);

        let app = Router::new()
            .route(
                "/res",
                get(move || {
                    let hits = Arc::clone(&hits_cc);
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        format!("hit-{n}")
                    }
                }),
            )
            .route(
                "/zero-max-age",
                get(|| async { ([(header::CACHE_CONTROL, "public, max-age=0")], "fresh") }),
            )
            .route(
                "/non-utf8-cc",
                get(|| async {
                    let mut res = "raw".into_response();
                    res.headers_mut().insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_bytes(b"public, \xff").unwrap(),
                    );
                    res
                }),
            )
            .layer(HttpCacheLayer::new(backend, HttpCacheConfig::default()));

        // 1. Request `Cache-Control: no-store` skips both read and write
        let r_ns = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/res")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_ns.into_body(), usize::MAX).await.unwrap(),
            b"hit-1".as_slice()
        );

        // Subsequent normal GET must miss (because no-store did not write) and populate cache -> hit-2
        let r_norm = app
            .clone()
            .oneshot(Request::builder().uri("/res").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_norm.into_body(), usize::MAX).await.unwrap(),
            b"hit-2".as_slice()
        );

        // 2. Request `Cache-Control: no-cache` skips cache read and refreshes cache -> hit-3
        let r_nc = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/res")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_nc.into_body(), usize::MAX).await.unwrap(),
            b"hit-3".as_slice()
        );

        // Subsequent normal GET now sees refreshed `hit-3`
        let r_cached = app
            .clone()
            .oneshot(Request::builder().uri("/res").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            to_bytes(r_cached.into_body(), usize::MAX).await.unwrap(),
            b"hit-3".as_slice()
        );

        // 3. Response `Cache-Control: max-age=0` and non-UTF-8 `Cache-Control` are not cached
        let mut h_zero = axum::http::HeaderMap::new();
        h_zero.insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=0"));
        assert!(!is_cacheable_response(&h_zero, &[]));

        let mut h_smax = axum::http::HeaderMap::new();
        h_smax.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, s-maxage=0"),
        );
        assert!(!is_cacheable_response(&h_smax, &[]));

        let mut h_non_utf8 = axum::http::HeaderMap::new();
        h_non_utf8.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
        );
        assert!(!is_cacheable_response(&h_non_utf8, &[]));

        // 4. Positive response `s-maxage` / `max-age` clamps backend TTL
        let mut h_clamp = axum::http::HeaderMap::new();
        h_clamp.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=120, s-maxage=15"),
        );
        assert_eq!(
            response_cache_ttl(&h_clamp, Duration::from_secs(60)),
            Duration::from_secs(15)
        );
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/zero-max-age")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let _ = app
            .oneshot(
                Request::builder()
                    .uri("/non-utf8-cc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
}
