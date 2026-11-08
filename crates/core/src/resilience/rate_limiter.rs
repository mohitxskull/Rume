//! Sliding-window rate limiter and Tower middleware.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Instant;

use axum::body::Body;
use axum::http::header::HeaderName;
use axum::http::{HeaderValue, Request, Response};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::resilience::error::ResilienceError;

/// Configuration for rate limiting policies.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum allowed requests within the time window.
    pub max_requests: u32,
    /// Duration of the sliding time window.
    pub window: Duration,
    /// Maximum active key entries per memory shard before inline eviction occurs (default: 10,000).
    pub max_entries_per_shard: usize,
    /// Trusted reverse proxy IP addresses. When non-empty and socket peer matches, X-Forwarded-For is honored.
    pub trusted_proxies: Vec<std::net::IpAddr>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window: Duration::from_secs(60),
            max_entries_per_shard: 10_000,
            trusted_proxies: Vec::new(),
        }
    }
}

impl RateLimitConfig {
    /// Sets the maximum allowed requests within the window.
    pub fn with_max_requests(mut self, max: u32) -> Self {
        self.max_requests = max;
        self
    }

    /// Sets the sliding time window duration.
    pub fn with_window(mut self, window: Duration) -> Self {
        self.window = window;
        self
    }

    /// Sets the maximum capacity of client keys tracked per memory shard.
    pub fn with_max_entries_per_shard(mut self, max: usize) -> Self {
        self.max_entries_per_shard = max.max(1);
        self
    }

    /// Adds a trusted reverse proxy IP address for client IP resolution.
    pub fn with_trusted_proxy(mut self, ip: std::net::IpAddr) -> Self {
        self.trusted_proxies.push(ip);
        self
    }

    /// Sets the list of trusted reverse proxy IP addresses.
    pub fn with_trusted_proxies(mut self, ips: Vec<std::net::IpAddr>) -> Self {
        self.trusted_proxies = ips;
        self
    }
}

/// Metadata describing rate limit status for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitStatus {
    pub limit: u32,
    pub remaining: u32,
    pub reset_after_secs: u64,
}

const NUM_SHARDS: usize = 16;

#[derive(Debug, Default)]
struct RateLimitEntry {
    timestamps: Vec<Instant>,
}

#[derive(Debug, Clone)]
enum RateLimiterBackend {
    InMemory(Arc<[RwLock<BTreeMap<String, RateLimitEntry>>]>),
    #[cfg(feature = "redis")]
    Redis(crate::resilience::redis::RedisRateLimiter),
}

/// In-memory or distributed Redis-backed sliding-window rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    config: RateLimitConfig,
    backend: RateLimiterBackend,
}

impl RateLimiter {
    /// Creates a new in-memory rate limiter with the specified configuration.
    pub fn new(config: RateLimitConfig) -> Self {
        let mut shards = Vec::with_capacity(NUM_SHARDS);
        for _ in 0..NUM_SHARDS {
            shards.push(RwLock::new(BTreeMap::new()));
        }
        Self {
            config,
            backend: RateLimiterBackend::InMemory(shards.into()),
        }
    }

    /// Creates a cluster-wide distributed rate limiter backed by Redis.
    #[cfg(feature = "redis")]
    pub fn redis(pool: deadpool_redis::Pool, config: RateLimitConfig) -> Self {
        Self {
            config: config.clone(),
            backend: RateLimiterBackend::Redis(crate::resilience::redis::RedisRateLimiter::new(
                pool, config,
            )),
        }
    }

    /// Returns a reference to the active rate limit configuration.
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    fn shard_for<'a>(
        shards: &'a [RwLock<BTreeMap<String, RateLimitEntry>>],
        key: &str,
    ) -> &'a RwLock<BTreeMap<String, RateLimitEntry>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % shards.len();
        &shards[idx]
    }

    fn check_in_memory(
        config: &RateLimitConfig,
        shards: &[RwLock<BTreeMap<String, RateLimitEntry>>],
        key: &str,
    ) -> Result<RateLimitStatus, ResilienceError> {
        let mut clients = Self::shard_for(shards, key).write().unwrap();
        let now = Instant::now();
        let window_start = now.checked_sub(config.window).unwrap_or(now);

        // Prune expired entries inline if shard is at capacity to prevent memory bloat
        if clients.len() >= config.max_entries_per_shard {
            clients.retain(|_, entry| {
                entry.timestamps.retain(|&t| t > window_start);
                !entry.timestamps.is_empty()
            });

            // If still at capacity, evict the entry with the oldest recorded timestamp to bound memory
            if clients.len() >= config.max_entries_per_shard
                && !clients.contains_key(key)
                && let Some(oldest_key) = clients
                    .iter()
                    .min_by_key(|(_, entry)| entry.timestamps.last().copied().unwrap_or(now))
                    .map(|(k, _)| k.clone())
            {
                clients.remove(&oldest_key);
            }
        }

        let entry = clients.entry(key.to_string()).or_default();
        // Discard timestamps older than the sliding window
        entry.timestamps.retain(|&t| t > window_start);

        if entry.timestamps.len() >= config.max_requests as usize {
            let oldest = entry.timestamps.first().copied().unwrap_or(now);
            let elapsed = now.duration_since(oldest);
            let retry_after = config.window.saturating_sub(elapsed).as_secs().max(1);

            return Err(ResilienceError::RateLimited {
                retry_after_secs: retry_after,
            });
        }

        entry.timestamps.push(now);
        let remaining = config
            .max_requests
            .saturating_sub(entry.timestamps.len() as u32);

        Ok(RateLimitStatus {
            limit: config.max_requests,
            remaining,
            reset_after_secs: config.window.as_secs(),
        })
    }

    /// Evaluates if a request from `key` is within limits, recording the request on success.
    pub fn check(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
        match &self.backend {
            RateLimiterBackend::InMemory(shards) => {
                Self::check_in_memory(&self.config, shards, key)
            }
            #[cfg(feature = "redis")]
            RateLimiterBackend::Redis(r) => tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(r.check(key))
            }),
        }
    }

    /// Asynchronously evaluates if a request from `key` is within limits (compatible with async Redis backends).
    pub async fn check_async(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
        match &self.backend {
            RateLimiterBackend::InMemory(shards) => {
                Self::check_in_memory(&self.config, shards, key)
            }
            #[cfg(feature = "redis")]
            RateLimiterBackend::Redis(r) => r.check(key).await,
        }
    }

    /// Prunes expired client entries whose recorded requests fall outside the sliding window.
    pub fn prune_expired(&self) {
        match &self.backend {
            RateLimiterBackend::InMemory(shards) => {
                let now = Instant::now();
                let window_start = now.checked_sub(self.config.window).unwrap_or(now);
                for shard in shards.iter() {
                    let mut clients = shard.write().unwrap();
                    clients.retain(|_, entry| {
                        entry.timestamps.retain(|&t| t > window_start);
                        !entry.timestamps.is_empty()
                    });
                }
            }
            #[cfg(feature = "redis")]
            RateLimiterBackend::Redis(_) => {}
        }
    }

    /// Spawns a background task that periodically prunes expired client entries at the given interval.
    pub fn spawn_background_eviction(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let limiter = self.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            loop {
                timer.tick().await;
                limiter.prune_expired();
            }
        })
    }
}

/// Tower middleware layer enforcing rate limits.
#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: RateLimiter,
}

impl RateLimitLayer {
    /// Creates a new rate limit layer.
    pub fn new(limiter: RateLimiter) -> Self {
        Self { limiter }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

/// Tower service for rate limiting.
#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: RateLimiter,
}

impl<S> Service<Request<Body>> for RateLimitService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let limiter = self.limiter.clone();
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        // Extract client key:
        // 1. Authenticated tenant context from request extensions (if present)
        let tenant_key = req
            .extensions()
            .get::<crate::tenant::Tenant>()
            .map(|t| format!("tenant:{}", t.id))
            .or_else(|| {
                req.extensions()
                    .get::<crate::tenant::TenantContext>()
                    .map(|tc| format!("tenant:{}", tc.id))
            });

        // 2. Otherwise derive client identity from socket peer (ConnectInfo) or trusted proxy
        let client_key = tenant_key.unwrap_or_else(|| {
            let socket_ip = req
                .extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip());

            let is_trusted_proxy = socket_ip
                .map(|ip| {
                    !limiter.config().trusted_proxies.is_empty()
                        && limiter.config().trusted_proxies.contains(&ip)
                })
                .unwrap_or(false);

            if is_trusted_proxy
                && let Some(forwarded_ip) = req
                    .headers()
                    .get("x-forwarded-for")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.split(',').next())
                    .map(|ip| ip.trim())
                    .filter(|ip| !ip.is_empty())
            {
                return format!("ip:{}", forwarded_ip);
            }

            socket_ip
                .map(|ip| format!("ip:{}", ip))
                .unwrap_or_else(|| "client_default".to_string())
        });

        Box::pin(async move {
            match limiter.check_async(&client_key).await {
                Ok(status) => {
                    let mut response = inner.call(req).await?;
                    let headers = response.headers_mut();
                    if let Ok(limit_val) = HeaderValue::from_str(&status.limit.to_string()) {
                        headers.insert(HeaderName::from_static("x-ratelimit-limit"), limit_val);
                    }
                    if let Ok(rem_val) = HeaderValue::from_str(&status.remaining.to_string()) {
                        headers.insert(HeaderName::from_static("x-ratelimit-remaining"), rem_val);
                    }
                    Ok(response)
                }
                Err(err) => {
                    let retry_secs = match &err {
                        ResilienceError::RateLimited { retry_after_secs } => *retry_after_secs,
                        _ => 60,
                    };

                    let app_err: AppError = err.into();
                    let mut response = app_err.into_response();
                    if let Ok(val) = HeaderValue::from_str(&retry_secs.to_string()) {
                        response
                            .headers_mut()
                            .insert(axum::http::header::RETRY_AFTER, val);
                    }
                    Ok(response)
                }
            }
        })
    }
}
