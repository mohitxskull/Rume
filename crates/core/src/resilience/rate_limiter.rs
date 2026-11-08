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
    /// Sentinel key used for unmapped identities when an in-memory shard reaches its maximum capacity.
    pub const OVERFLOW_KEY: &'static str = "__overflow__";

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

        let target_key = if clients.contains_key(key) {
            // Active known identity: always enforce its recorded history.
            key
        } else {
            // 1. Prune expired entries inline if shard is at capacity to free up inactive slots.
            if clients.len() >= config.max_entries_per_shard {
                clients.retain(|_, entry| {
                    entry.timestamps.retain(|&t| t > window_start);
                    !entry.timestamps.is_empty()
                });
            }

            // 2. SECURITY INVARIANT (DO NOT EVICT ACTIVE ENTRIES):
            // In naive rate-limit implementations, when a cache is full, the oldest entry is evicted
            // to make room for new keys. Under high cardinality attacks (e.g. an attacker rotating forged
            // client IPs or IDs), evicting the oldest entry wipes out that client's rate-limiting history!
            // The attacker can then immediately reuse that client identity with a fresh quota, completely
            // bypassing rate limits via cache-churn eviction.
            //
            // Fix: When the shard remains at capacity after pruning expired entries, any new unmapped identity
            // is routed to the shared `__overflow__` bucket. This bounds memory strictly to `max_entries_per_shard + 1`,
            // throttles excess unmapped traffic as a shared group, and NEVER resets the rate limits of active clients.
            if clients.len() >= config.max_entries_per_shard && !clients.contains_key(key) {
                Self::OVERFLOW_KEY
            } else {
                key
            }
        };

        let entry = clients.entry(target_key.to_string()).or_default();
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

            let forwarded_header = req
                .headers()
                .get("x-forwarded-for")
                .and_then(|h| h.to_str().ok());

            resolve_forwarded_client_ip(
                socket_ip,
                forwarded_header,
                &limiter.config().trusted_proxies,
            )
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

/// Resolves the genuine client IP address from the direct socket peer and optional `X-Forwarded-For` chain.
///
/// # Security and Proxy Invariants (CRITICAL — DO NOT USE `s.split(',').next()`)
///
/// ## The Spoofing Vulnerability in Naive Parsers:
/// An HTTP client connects to an edge proxy (e.g., Cloudflare, ALB, Nginx). If the client sends an initial header
/// `X-Forwarded-For: 1.1.1.1`, the proxy appends the client's actual IP (`203.0.113.195`), producing:
/// `X-Forwarded-For: 1.1.1.1, 203.0.113.195`.
///
/// A naive parser that takes the first IP (`s.split(',').next()`) trusts the attacker-controlled `1.1.1.1`.
/// The attacker can rotate this header on every request to obtain a fresh rate limit bucket, completely bypassing
/// IP-based rate limiting, geofencing, and security auditing!
///
/// ## Secure Right-to-Left (RTL) Proxy Chain Evaluation:
/// Intermediate reverse proxies append IPs to the **right** of `X-Forwarded-For`.
/// 1. If the direct socket peer (`socket_ip`) is missing or not in `trusted_proxies`, `X-Forwarded-For` is completely ignored.
///    The untrusted socket peer is used directly as the client identity.
/// 2. If the direct socket peer is in `trusted_proxies`, `X-Forwarded-For` is parsed from right to left (`rsplit(',')`).
/// 3. Any IP belonging to `trusted_proxies` is an internal proxy hop and is safely skipped.
/// 4. The **first untrusted IP** encountered from the right is the genuine client IP that connected to our trusted proxy edge.
/// 5. If all IPs in the chain belong to trusted proxies, the leftmost valid IP is used as the originator.
pub fn resolve_forwarded_client_ip(
    socket_ip: Option<std::net::IpAddr>,
    forwarded_header: Option<&str>,
    trusted_proxies: &[std::net::IpAddr],
) -> String {
    let Some(peer) = socket_ip else {
        return "client_default".to_string();
    };

    if trusted_proxies.is_empty() || !trusted_proxies.contains(&peer) {
        return format!("ip:{peer}");
    }

    if let Some(chain) = forwarded_header {
        let mut last_valid_ip = None;
        for hop in chain.rsplit(',') {
            let trimmed = hop.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
                if !trusted_proxies.contains(&ip) {
                    return format!("ip:{ip}");
                }
                last_valid_ip = Some(ip);
            }
        }
        if let Some(leftmost) = last_valid_ip {
            return format!("ip:{leftmost}");
        }
    }

    format!("ip:{peer}")
}
