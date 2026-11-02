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
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window: Duration::from_secs(60),
        }
    }
}

/// Metadata describing rate limit status for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitStatus {
    pub limit: u32,
    pub remaining: u32,
    pub reset_after_secs: u64,
}

#[derive(Debug, Default)]
struct RateLimitEntry {
    timestamps: Vec<Instant>,
}

/// In-memory sliding-window rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    config: RateLimitConfig,
    clients: Arc<RwLock<BTreeMap<String, RateLimitEntry>>>,
}

impl RateLimiter {
    /// Creates a new rate limiter with the specified configuration.
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            clients: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Evaluates if a request from `key` is within limits, recording the request on success.
    pub fn check(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
        let mut clients = self.clients.write().unwrap();
        let now = Instant::now();
        let window_start = now.checked_sub(self.config.window).unwrap_or(now);

        let entry = clients.entry(key.to_string()).or_default();
        // Discard timestamps older than the sliding window
        entry.timestamps.retain(|&t| t > window_start);

        if entry.timestamps.len() >= self.config.max_requests as usize {
            let oldest = entry.timestamps.first().copied().unwrap_or(now);
            let elapsed = now.duration_since(oldest);
            let retry_after = self.config.window.saturating_sub(elapsed).as_secs().max(1);

            return Err(ResilienceError::RateLimited {
                retry_after_secs: retry_after,
            });
        }

        entry.timestamps.push(now);
        let remaining = self
            .config
            .max_requests
            .saturating_sub(entry.timestamps.len() as u32);

        // Passive eviction of inactive clients to bound memory usage
        if clients.len() > 256 {
            clients.retain(|_, e| {
                e.timestamps.retain(|&t| t > window_start);
                !e.timestamps.is_empty()
            });
        }

        Ok(RateLimitStatus {
            limit: self.config.max_requests,
            remaining,
            reset_after_secs: self.config.window.as_secs(),
        })
    }

    /// Prunes expired client entries whose recorded requests fall outside the sliding window.
    pub fn prune_expired(&self) {
        let mut clients = self.clients.write().unwrap();
        let now = Instant::now();
        let window_start = now.checked_sub(self.config.window).unwrap_or(now);
        clients.retain(|_, entry| {
            entry.timestamps.retain(|&t| t > window_start);
            !entry.timestamps.is_empty()
        });
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
        let mut inner = self.inner.clone();

        // Extract client key (from X-Forwarded-For or default "global")
        let client_key = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("client_default")
            .to_string();

        Box::pin(async move {
            match limiter.check(&client_key) {
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
