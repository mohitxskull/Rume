//! Instrumented outbound HTTP client ([`RumeClient`]) with W3C trace propagation,
//! circuit breaking, retries, and timeouts.
//!
//! Service discovery abstractions ([`ServiceDiscovery`], [`StaticDiscovery`], and
//! [`DnsSrvDiscovery`] under the `dns-srv` feature) live in [`discovery`] so callers
//! can resolve target endpoints before building requests.

pub mod discovery;

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Request, Response};
use thiserror::Error;

use crate::resilience::circuit_breaker::{CircuitBreaker, TrialPermit};
#[cfg(feature = "dns-srv")]
pub use discovery::DnsSrvDiscovery;
pub use discovery::{ServiceDiscovery, ServiceEndpoint, StaticDiscovery};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    connect_timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    redirect_policy: Option<reqwest::redirect::Policy>,
}

impl RumeClientBuilder {
    /// Creates a new [`RumeClientBuilder`] with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses a pre-configured `reqwest::Client`.
    ///
    /// Passing a pre-built `reqwest::Client` overrides any [`Self::connect_timeout`],
    /// [`Self::request_timeout`], and [`Self::redirect_policy`] settings configured on this
    /// builder (a `tracing::warn!` is emitted at [`Self::build`] time if both are set).
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

    /// Sets the TCP/TLS connect timeout on the underlying `reqwest::Client` (default: 10s).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the request timeout on the underlying `reqwest::Client` (default: 30s).
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Sets the HTTP redirect policy on the underlying `reqwest::Client` (default: `Policy::none()`).
    pub fn redirect_policy(mut self, policy: reqwest::redirect::Policy) -> Self {
        self.redirect_policy = Some(policy);
        self
    }

    /// Builds the [`RumeClient`].
    pub fn build(self) -> Result<RumeClient, OutboundHttpError> {
        let inner = match self.client {
            Some(client) => {
                if self.connect_timeout.is_some()
                    || self.request_timeout.is_some()
                    || self.redirect_policy.is_some()
                {
                    tracing::warn!(
                        "RumeClientBuilder::client() overrides builder connect_timeout, request_timeout, and redirect_policy settings"
                    );
                }
                client
            }
            None => reqwest::Client::builder()
                .redirect(
                    self.redirect_policy
                        .unwrap_or_else(reqwest::redirect::Policy::none),
                )
                .connect_timeout(self.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT))
                .timeout(self.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT))
                .build()?,
        };
        Ok(RumeClient {
            inner,
            circuit_breaker: self.circuit_breaker,
            retry: self.retry,
            attempt_timeout: self.attempt_timeout,
            total_timeout: self.total_timeout,
        })
    }
}

/// Instrumented outbound HTTP client wrapping `reqwest::Client`.
#[derive(Debug, Clone)]
pub struct RumeClient {
    inner: reqwest::Client,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
    retry: RetryConfig,
    attempt_timeout: Option<Duration>,
    total_timeout: Option<Duration>,
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
    pub fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.inner.request(method, url)
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
    pub async fn execute(&self, mut request: Request) -> Result<Response, OutboundHttpError> {
        inject_telemetry_headers(request.headers_mut());

        let active_permit = parking_lot::Mutex::new(None);
        match self.total_timeout {
            Some(total) => match tokio::time::timeout(
                total,
                self.execute_with_retries(request, &active_permit),
            )
            .await
            {
                Ok(res) => res,
                Err(_) => {
                    // Why: `tokio::time::timeout` cancels the inner future before `execute_single_attempt`
                    // can report failure; explicitly complete the held permit so the circuit breaker trips.
                    if let Some(permit) = active_permit.lock().take() {
                        permit.complete_failure();
                    }
                    Err(OutboundHttpError::Timeout)
                }
            },
            None => self.execute_with_retries(request, &active_permit).await,
        }
    }

    async fn execute_with_retries(
        &self,
        request: Request,
        active_permit: &parking_lot::Mutex<Option<TrialPermit>>,
    ) -> Result<Response, OutboundHttpError> {
        let max_attempts = self.retry.max_attempts.max(1);
        let can_retry_method =
            is_idempotent_method(request.method()) || self.retry.retry_non_idempotent;
        let mut pending = Some(request);

        for attempt in 1..=max_attempts {
            let current_request = pending.take().expect("request must be present for attempt");
            let want_retry = attempt < max_attempts && can_retry_method;
            let (attempt_request, can_retry_after) = if want_retry {
                match current_request.try_clone() {
                    Some(cloned) => {
                        pending = Some(current_request);
                        (cloned, true)
                    }
                    None => (current_request, false),
                }
            } else {
                (current_request, false)
            };

            match self
                .execute_single_attempt(attempt_request, active_permit)
                .await
            {
                Ok(resp) if !resp.status().is_server_error() || !can_retry_after => {
                    return Ok(resp);
                }
                Ok(_) => {}
                Err(OutboundHttpError::CircuitOpen) => return Err(OutboundHttpError::CircuitOpen),
                Err(err) if !can_retry_after => return Err(err),
                Err(_) => {}
            }

            let delay =
                full_jitter_backoff(self.retry.initial_backoff, self.retry.max_backoff, attempt);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }

        unreachable!("loop always returns on the final attempt")
    }

    async fn execute_single_attempt(
        &self,
        request: Request,
        active_permit: &parking_lot::Mutex<Option<TrialPermit>>,
    ) -> Result<Response, OutboundHttpError> {
        if let Some(ref cb) = self.circuit_breaker {
            let permit = cb.try_acquire().ok_or(OutboundHttpError::CircuitOpen)?;
            *active_permit.lock() = Some(permit);
        }

        let result = match self.attempt_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, self.inner.execute(request)).await
            {
                Ok(res) => res.map_err(OutboundHttpError::Transport),
                Err(_) => Err(OutboundHttpError::Timeout),
            },
            None => self
                .inner
                .execute(request)
                .await
                .map_err(OutboundHttpError::Transport),
        };

        let permit = active_permit.lock().take();
        match result {
            Ok(resp) => {
                if let Some(p) = permit {
                    if resp.status().is_server_error() {
                        p.complete_failure();
                    } else {
                        p.complete_success();
                    }
                }
                Ok(resp)
            }
            Err(err) => {
                if let Some(p) = permit {
                    p.complete_failure();
                }
                Err(err)
            }
        }
    }
}

fn is_idempotent_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
    )
}

fn full_jitter_backoff(initial: Duration, max: Duration, attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1);
    let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    let cap = initial.saturating_mul(factor).min(max);
    let max_nanos = cap.as_nanos().min(u128::from(u64::MAX)) as u64;
    if max_nanos == 0 {
        Duration::ZERO
    } else {
        Duration::from_nanos(rand::random_range(0..=max_nanos))
    }
}

fn inject_telemetry_headers(headers: &mut reqwest::header::HeaderMap) {
    use rume_foundation::telemetry::{RequestId, X_REQUEST_ID};
    use rume_observability::trace_context::{TRACEPARENT, TRACESTATE, TraceContext};

    if !headers.contains_key(X_REQUEST_ID.as_str())
        && let Ok(val) = reqwest::header::HeaderValue::from_str(RequestId::generate().as_str())
    {
        headers.insert(
            reqwest::header::HeaderName::from_static("x-request-id"),
            val,
        );
    }

    if !headers.contains_key(TRACEPARENT.as_str())
        && let Some(tc) = TraceContext::current()
    {
        if let Ok(tp_val) = reqwest::header::HeaderValue::from_str(&tc.traceparent) {
            headers.insert(
                reqwest::header::HeaderName::from_static("traceparent"),
                tp_val,
            );
        }
        if let Some(ts) = tc.tracestate
            && !headers.contains_key(TRACESTATE.as_str())
            && let Ok(ts_val) = reqwest::header::HeaderValue::from_str(&ts)
        {
            headers.insert(
                reqwest::header::HeaderName::from_static("tracestate"),
                ts_val,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use rume_foundation::telemetry::X_REQUEST_ID;
    use rume_observability::trace_context::{TRACEPARENT, TRACESTATE, TraceContext};
    use tokio::net::TcpListener;

    use crate::resilience::circuit_breaker::{CircuitBreakerConfig, CircuitState};

    async fn spawn_test_server(app: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn rume_client_injects_traceparent_tracestate_and_request_id() {
        let captured = Arc::new(parking_lot::Mutex::new(None::<HeaderMap>));
        let captured_clone = Arc::clone(&captured);

        let app = Router::new().route(
            "/echo",
            get(move |headers: HeaderMap| {
                let captured = Arc::clone(&captured_clone);
                async move {
                    *captured.lock() = Some(headers);
                    StatusCode::OK
                }
            }),
        );
        let base_url = spawn_test_server(app).await;
        let client = RumeClient::new().expect("build client");

        let tc = TraceContext {
            traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
            tracestate: Some("rojo=00f067aa0ba902b7".to_string()),
        };

        let resp = tc
            .scope_future(tracing::info_span!("outbound_test"), async {
                let req = client
                    .get(&format!("{base_url}/echo"))
                    .build()
                    .expect("build request");
                client.execute(req).await
            })
            .await
            .expect("execute request");

        assert_eq!(resp.status(), StatusCode::OK);
        let headers = captured.lock().clone().expect("headers captured");
        assert_eq!(
            headers
                .get(TRACEPARENT.as_str())
                .and_then(|v| v.to_str().ok()),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        assert_eq!(
            headers
                .get(TRACESTATE.as_str())
                .and_then(|v| v.to_str().ok()),
            Some("rojo=00f067aa0ba902b7")
        );
        let req_id = headers
            .get(X_REQUEST_ID.as_str())
            .and_then(|v| v.to_str().ok())
            .expect("x-request-id header present");
        assert!(!req_id.is_empty());
    }

    #[tokio::test]
    async fn rume_client_retries_idempotent_get_with_backoff_and_succeeds() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let app = Router::new().route(
            "/flaky",
            get(move || {
                let attempts = Arc::clone(&attempts_clone);
                async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst);
                    if n < 2 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        );
        let base_url = spawn_test_server(app).await;
        let client = RumeClient::builder()
            .retry(RetryConfig {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(5),
                max_backoff: Duration::from_millis(20),
                retry_non_idempotent: false,
            })
            .build()
            .expect("build client");

        let req = client
            .get(&format!("{base_url}/flaky"))
            .build()
            .expect("build request");
        let resp = client.execute(req).await.expect("execute request");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn rume_client_does_not_retry_post_unless_opted_in() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let app = Router::new().route(
            "/submit",
            post(move || {
                let attempts = Arc::clone(&attempts_clone);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        );
        let base_url = spawn_test_server(app).await;

        let default_client = RumeClient::builder()
            .retry(RetryConfig {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(5),
                max_backoff: Duration::from_millis(20),
                retry_non_idempotent: false,
            })
            .build()
            .expect("build default client");

        let req = default_client
            .post(&format!("{base_url}/submit"))
            .build()
            .expect("build post");
        let resp = default_client
            .execute(req)
            .await
            .expect("execute default post");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        attempts.store(0, Ordering::SeqCst);
        let retry_client = RumeClient::builder()
            .retry(RetryConfig {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(5),
                max_backoff: Duration::from_millis(20),
                retry_non_idempotent: false,
            })
            .retry_non_idempotent(true)
            .build()
            .expect("build retry post client");

        let req = retry_client
            .post(&format!("{base_url}/submit"))
            .build()
            .expect("build post");
        let resp = retry_client.execute(req).await.expect("execute retry post");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn rume_client_trips_circuit_breaker_and_recovers() {
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let fail_clone = Arc::clone(&fail);

        let app = Router::new().route(
            "/cb",
            get(move || {
                let fail = Arc::clone(&fail_clone);
                async move {
                    if fail.load(Ordering::SeqCst) {
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        );
        let base_url = spawn_test_server(app).await;

        let breaker = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 1,
            reset_timeout: Duration::from_millis(50),
            ..Default::default()
        }));
        let client = RumeClient::builder()
            .circuit_breaker(Arc::clone(&breaker))
            .retry(RetryConfig {
                max_attempts: 1,
                ..RetryConfig::default()
            })
            .build()
            .expect("build client");

        for _ in 0..2 {
            let req = client
                .get(&format!("{base_url}/cb"))
                .build()
                .expect("build req");
            let resp = client.execute(req).await.expect("500 response");
            assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }

        assert_eq!(breaker.state(), CircuitState::Open);

        let req = client
            .get(&format!("{base_url}/cb"))
            .build()
            .expect("build req");
        let err = client
            .execute(req)
            .await
            .expect_err("expected circuit open");
        assert!(matches!(err, OutboundHttpError::CircuitOpen));

        fail.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(65)).await;

        let req = client
            .get(&format!("{base_url}/cb"))
            .build()
            .expect("build req");
        let resp = client.execute(req).await.expect("recovery probe");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn rume_client_enforces_total_timeout() {
        let app = Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                StatusCode::OK
            }),
        );
        let base_url = spawn_test_server(app).await;

        let client = RumeClient::builder()
            .total_timeout(Duration::from_millis(40))
            .build()
            .expect("build client");

        let req = client
            .get(&format!("{base_url}/slow"))
            .build()
            .expect("build req");
        let err = client
            .execute(req)
            .await
            .expect_err("expected total timeout");
        assert!(matches!(err, OutboundHttpError::Timeout));
    }

    #[tokio::test]
    async fn rume_client_total_timeout_trips_circuit_breaker() {
        let app = Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                StatusCode::OK
            }),
        );
        let base_url = spawn_test_server(app).await;

        let breaker = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            reset_timeout: Duration::from_secs(30),
            ..Default::default()
        }));
        let client = RumeClient::builder()
            .circuit_breaker(Arc::clone(&breaker))
            .total_timeout(Duration::from_millis(40))
            .build()
            .expect("build client");

        let req = client
            .get(&format!("{base_url}/slow"))
            .build()
            .expect("build req");
        let err = client
            .execute(req)
            .await
            .expect_err("expected total timeout");
        assert!(matches!(err, OutboundHttpError::Timeout));
        assert_eq!(breaker.state(), CircuitState::Open);
    }

    #[tokio::test]
    async fn rume_client_does_not_follow_redirects_by_default() {
        let target_hits = Arc::new(AtomicUsize::new(0));
        let target_hits_clone = Arc::clone(&target_hits);

        let app = Router::new()
            .route(
                "/redirect",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [(axum::http::header::LOCATION, "/internal-secret")],
                    )
                }),
            )
            .route(
                "/internal-secret",
                get(move || {
                    let target_hits = Arc::clone(&target_hits_clone);
                    async move {
                        target_hits.fetch_add(1, Ordering::SeqCst);
                        StatusCode::OK
                    }
                }),
            );
        let base_url = spawn_test_server(app).await;
        let client = RumeClient::new().expect("build client");

        let req = client
            .get(&format!("{base_url}/redirect"))
            .build()
            .expect("build req");
        let resp = client.execute(req).await.expect("execute redirect");
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }
}
