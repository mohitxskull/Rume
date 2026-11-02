//! Circuit breaker pattern implementation and Tower middleware.

use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Response};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::resilience::error::ResilienceError;

/// Operational state of a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation: requests pass through.
    Closed,
    /// Downstream failures exceeded threshold: requests are immediately rejected.
    Open,
    /// Probing health: trial requests are allowed to test recovery.
    HalfOpen,
}

/// Configuration settings for a circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit.
    pub failure_threshold: u32,
    /// Number of consecutive successes in HalfOpen state required to close the circuit.
    pub success_threshold: u32,
    /// Cooldown duration before transitioning from Open to HalfOpen.
    pub reset_timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            reset_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug)]
struct CircuitInner {
    state: CircuitState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    opened_at: Option<Instant>,
    half_open_in_flight: bool,
}

/// A circuit breaker tracking execution outcomes and preventing cascading failures.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    inner: Arc<RwLock<CircuitInner>>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

impl CircuitBreaker {
    /// Creates a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            inner: Arc::new(RwLock::new(CircuitInner {
                state: CircuitState::Closed,
                consecutive_failures: 0,
                consecutive_successes: 0,
                opened_at: None,
                half_open_in_flight: false,
            })),
        }
    }

    /// Returns the current state of the circuit breaker.
    pub fn state(&self) -> CircuitState {
        let mut inner = self.inner.write().unwrap();
        if inner.state == CircuitState::Open
            && let Some(opened) = inner.opened_at
            && opened.elapsed() >= self.config.reset_timeout
        {
            inner.state = CircuitState::HalfOpen;
            inner.consecutive_successes = 0;
            inner.half_open_in_flight = false;
        }
        inner.state
    }

    /// Evaluates whether a new request is permitted to execute.
    pub fn allow_request(&self) -> bool {
        let mut inner = self.inner.write().unwrap();
        if inner.state == CircuitState::Open
            && let Some(opened) = inner.opened_at
            && opened.elapsed() >= self.config.reset_timeout
        {
            inner.state = CircuitState::HalfOpen;
            inner.consecutive_successes = 0;
            inner.half_open_in_flight = false;
        }

        match inner.state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => {
                if inner.half_open_in_flight {
                    false
                } else {
                    inner.half_open_in_flight = true;
                    true
                }
            }
            CircuitState::Open => false,
        }
    }

    /// Records a successful execution.
    pub fn on_success(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.half_open_in_flight = false;
        match inner.state {
            CircuitState::HalfOpen => {
                inner.consecutive_successes += 1;
                if inner.consecutive_successes >= self.config.success_threshold {
                    inner.state = CircuitState::Closed;
                    inner.consecutive_failures = 0;
                    inner.consecutive_successes = 0;
                    inner.opened_at = None;
                }
            }
            CircuitState::Closed => {
                inner.consecutive_failures = 0;
            }
            CircuitState::Open => {}
        }
    }

    /// Records a failed execution.
    pub fn on_failure(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.half_open_in_flight = false;
        match inner.state {
            CircuitState::Closed => {
                inner.consecutive_failures += 1;
                if inner.consecutive_failures >= self.config.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(Instant::now());
                }
            }
            CircuitState::HalfOpen => {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(Instant::now());
            }
            CircuitState::Open => {}
        }
    }
}

/// Tower layer applying a circuit breaker guard to requests.
#[derive(Clone)]
pub struct CircuitBreakerLayer {
    breaker: CircuitBreaker,
}

impl CircuitBreakerLayer {
    /// Creates a new circuit breaker layer.
    pub fn new(breaker: CircuitBreaker) -> Self {
        Self { breaker }
    }
}

impl<S> Layer<S> for CircuitBreakerLayer {
    type Service = CircuitBreakerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CircuitBreakerService {
            inner,
            breaker: self.breaker.clone(),
        }
    }
}

/// Tower service wrapping inner handler with circuit breaker enforcement.
#[derive(Clone)]
pub struct CircuitBreakerService<S> {
    inner: S,
    breaker: CircuitBreaker,
}

impl<S> Service<Request<Body>> for CircuitBreakerService<S>
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
        let breaker = self.breaker.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            if !breaker.allow_request() {
                let err: AppError = ResilienceError::CircuitOpen.into();
                return Ok(err.into_response());
            }

            let response = match inner.call(req).await {
                Ok(res) => res,
                Err(err) => {
                    breaker.on_failure();
                    return Err(err);
                }
            };

            if response.status().is_server_error() {
                breaker.on_failure();
            } else {
                breaker.on_success();
            }

            Ok(response)
        })
    }
}
