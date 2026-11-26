//! Instrumented outbound HTTP client (`RumeClient`) with W3C trace propagation,
//! circuit breaking, retries, timeouts, and service discovery.

pub mod discovery;

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Request, Response};
use thiserror::Error;

use crate::resilience::circuit_breaker::CircuitBreaker;
#[cfg(feature = "dns-srv")]
pub use discovery::DnsSrvDiscovery;
pub use discovery::{ServiceDiscovery, ServiceEndpoint, StaticDiscovery};

/// Errors returned by [`RumeClient`] outbound HTTP operations.
#[derive(Debug, Error)]
pub enum OutboundHttpError {
    /// Underlying `reqwest` transport or request-building failure.
    #[error("http transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// Request rejected because the [`CircuitBreaker`] is open.
    #[error("circuit breaker open")]
    CircuitOpen,
    /// Request exceeded the configured total or per-attempt timeout.
    #[error("outbound http request timed out")]
    Timeout,
    /// Service discovery failed to resolve any endpoints.
    #[error("service discovery error: {0}")]
    Discovery(String),
}

/// Retry configuration for [`RumeClient`] with exponential backoff and full jitter.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts (including the initial attempt).
    pub max_attempts: u32,
    /// Base backoff duration before jitter.
    pub initial_backoff: Duration,
    /// Maximum backoff cap between attempts.
    pub max_backoff: Duration,
    /// Whether non-idempotent HTTP methods (e.g. `POST`, `PATCH`) may be retried.
    pub retry_non_idempotent: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(5),
            retry_non_idempotent: false,
        }
    }
}

/// Builder for [`RumeClient`].
#[derive(Debug, Default)]
pub struct RumeClientBuilder {
    client: Option<reqwest::Client>,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
    retry: RetryConfig,
    attempt_timeout: Option<Duration>,
    total_timeout: Option<Duration>,
}

impl RumeClientBuilder {
    /// Creates a new [`RumeClientBuilder`] with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses a pre-configured `reqwest::Client`.
    pub fn client(mut self, client: reqwest::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Attaches a [`CircuitBreaker`] to guard outbound calls.
    pub fn circuit_breaker(mut self, circuit_breaker: Arc<CircuitBreaker>) -> Self {
        self.circuit_breaker = Some(circuit_breaker);
        self
    }

    /// Configures retry behavior.
    pub fn retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Sets whether non-idempotent methods are retried.
    pub fn retry_non_idempotent(mut self, enabled: bool) -> Self {
        self.retry.retry_non_idempotent = enabled;
        self
    }

    /// Sets the per-attempt timeout.
    pub fn attempt_timeout(mut self, timeout: Duration) -> Self {
        self.attempt_timeout = Some(timeout);
        self
    }

    /// Sets the overall timeout across all retry attempts.
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = Some(timeout);
        self
    }

    /// Builds the [`RumeClient`].
    pub fn build(self) -> Result<RumeClient, OutboundHttpError> {
        todo!("Task 04: implement RumeClientBuilder::build")
    }
}

/// Instrumented outbound HTTP client wrapping `reqwest::Client`.
#[derive(Debug, Clone)]
pub struct RumeClient {
    inner: reqwest::Client,
    _circuit_breaker: Option<Arc<CircuitBreaker>>,
    _retry: RetryConfig,
    _attempt_timeout: Option<Duration>,
    _total_timeout: Option<Duration>,
}

impl RumeClient {
    /// Starts building a [`RumeClient`].
    pub fn builder() -> RumeClientBuilder {
        RumeClientBuilder::new()
    }

    /// Creates a [`RumeClient`] with default configuration.
    pub fn new() -> Result<Self, OutboundHttpError> {
        Self::builder().build()
    }

    /// Returns a reference to the underlying `reqwest::Client`.
    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    /// Starts building a request with the given HTTP `method` and `url`.
    pub fn request(&self, _method: Method, _url: &str) -> reqwest::RequestBuilder {
        todo!("Task 04: implement RumeClient::request")
    }

    /// Convenience `GET` request builder.
    pub fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Convenience `POST` request builder.
    pub fn post(&self, url: &str) -> reqwest::RequestBuilder {
        self.request(Method::POST, url)
    }

    /// Convenience `PUT` request builder.
    pub fn put(&self, url: &str) -> reqwest::RequestBuilder {
        self.request(Method::PUT, url)
    }

    /// Convenience `PATCH` request builder.
    pub fn patch(&self, url: &str) -> reqwest::RequestBuilder {
        self.request(Method::PATCH, url)
    }

    /// Convenience `DELETE` request builder.
    pub fn delete(&self, url: &str) -> reqwest::RequestBuilder {
        self.request(Method::DELETE, url)
    }

    /// Executes a `reqwest::Request` with trace propagation, circuit breaking, retries, and timeouts.
    pub async fn execute(&self, _request: Request) -> Result<Response, OutboundHttpError> {
        todo!("Task 04: implement RumeClient::execute")
    }
}
