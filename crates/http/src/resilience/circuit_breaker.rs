//! Circuit breaker pattern implementation and Tower middleware.

use parking_lot::RwLock;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Instant;

use axum::body::Body;
use axum::http::{Request, Response};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::resilience::error::ResilienceError;
use rume_foundation::error::AppError;

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
    half_open_generation: u64,
    active_half_open_permit: Option<u64>,
}

/// Drop-guarded permit for an in-flight request admitted by [`CircuitBreaker`].
///
/// For `HalfOpen` trials, carries the trial's generation ID and releases the
/// `half_open_in_flight` slot on drop if the future is cancelled before
/// reporting completion.
#[derive(Debug)]
pub struct TrialPermit {
    breaker: CircuitBreaker,
    generation: Option<u64>,
    completed: bool,
}

impl TrialPermit {
    /// Marks the permitted execution as successful, updating the circuit breaker state.
    pub fn complete_success(mut self) {
        self.completed = true;
        self.breaker.complete_with_permit(self.generation, true);
    }

    /// Marks the permitted execution as failed, updating the circuit breaker state.
    pub fn complete_failure(mut self) {
        self.completed = true;
        self.breaker.complete_with_permit(self.generation, false);
    }
}

impl Drop for TrialPermit {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Some(generation) = self.generation {
            // Why: `parking_lot::RwLock` does not poison, so `TrialPermit::drop` can never
            // double-panic and abort the process if dropped during stack unwinding (`RC-03`).
            let mut inner = self.breaker.inner.write();
            if inner.state == CircuitState::HalfOpen
                && inner.active_half_open_permit == Some(generation)
            {
                inner.half_open_in_flight = false;
                inner.active_half_open_permit = None;
            }
        }
    }
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
                half_open_generation: 0,
                active_half_open_permit: None,
            })),
        }
    }

    /// Returns the current state of the circuit breaker.
    pub fn state(&self) -> CircuitState {
        let mut inner = self.inner.write();
        Self::try_transition_to_half_open(&mut inner, self.config.reset_timeout);
        inner.state
    }

    /// Attempts to acquire a Drop-guarded [`TrialPermit`] for executing a request.
    ///
    /// Returns `None` when the circuit is `Open` or when a `HalfOpen` trial probe is already
    /// in flight. Dropping the returned [`TrialPermit`] without calling
    /// [`TrialPermit::complete_success`] or [`TrialPermit::complete_failure`] automatically
    /// releases the `HalfOpen` trial slot so cancelled futures cannot wedge the circuit.
    pub fn try_acquire(&self) -> Option<TrialPermit> {
        let mut inner = self.inner.write();
        Self::try_transition_to_half_open(&mut inner, self.config.reset_timeout);

        match inner.state {
            CircuitState::Closed => Some(TrialPermit {
                breaker: self.clone(),
                generation: None,
                completed: false,
            }),
            CircuitState::HalfOpen => {
                if inner.half_open_in_flight {
                    None
                } else {
                    let generation = inner.half_open_generation.wrapping_add(1);
                    inner.half_open_generation = generation;
                    inner.active_half_open_permit = Some(generation);
                    inner.half_open_in_flight = true;
                    inner.opened_at = Some(Instant::now());
                    Some(TrialPermit {
                        breaker: self.clone(),
                        generation: Some(generation),
                        completed: false,
                    })
                }
            }
            CircuitState::Open => None,
        }
    }

    fn complete_with_permit(&self, permit_generation: Option<u64>, success: bool) {
        let mut inner = self.inner.write();
        match permit_generation {
            Some(generation) => {
                if inner.state != CircuitState::HalfOpen
                    || inner.active_half_open_permit != Some(generation)
                {
                    return;
                }
                inner.half_open_in_flight = false;
                inner.active_half_open_permit = None;
                if success {
                    inner.consecutive_successes += 1;
                    if inner.consecutive_successes >= self.config.success_threshold {
                        inner.state = CircuitState::Closed;
                        inner.consecutive_failures = 0;
                        inner.consecutive_successes = 0;
                        inner.opened_at = None;
                    }
                } else {
                    inner.state = CircuitState::Open;
                    inner.consecutive_successes = 0;
                    inner.opened_at = Some(Instant::now());
                }
            }
            None => {
                if inner.state == CircuitState::Closed {
                    if success {
                        inner.consecutive_failures = 0;
                    } else {
                        inner.consecutive_failures += 1;
                        if inner.consecutive_failures >= self.config.failure_threshold {
                            inner.state = CircuitState::Open;
                            inner.opened_at = Some(Instant::now());
                        }
                    }
                }
            }
        }
    }

    /// Transitions Open → HalfOpen when the reset timeout has elapsed.
    fn try_transition_to_half_open(inner: &mut CircuitInner, reset_timeout: Duration) {
        if inner.state == CircuitState::Open
            && let Some(opened) = inner.opened_at
            && opened.elapsed() >= reset_timeout
        {
            inner.state = CircuitState::HalfOpen;
            inner.consecutive_successes = 0;
            inner.opened_at = Some(Instant::now()); // mark HalfOpen entry time
            inner.half_open_in_flight = false;
            inner.active_half_open_permit = None;
        }
    }

    /// Records a successful execution.
    pub fn on_success(&self) {
        let mut inner = self.inner.write();
        inner.half_open_in_flight = false;
        inner.active_half_open_permit = None;
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
        let mut inner = self.inner.write();
        inner.half_open_in_flight = false;
        inner.active_half_open_permit = None;
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
                inner.consecutive_successes = 0;
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

impl<S> CircuitBreakerService<S> {
    pub fn new(inner: S, breaker: CircuitBreaker) -> Self {
        Self { inner, breaker }
    }
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
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let Some(permit) = breaker.try_acquire() else {
                let err: AppError = ResilienceError::CircuitOpen.into();
                return Ok(err.into_response());
            };

            let response = match inner.call(req).await {
                Ok(res) => res,
                Err(err) => {
                    permit.complete_failure();
                    return Err(err);
                }
            };

            if response.status().is_server_error() {
                permit.complete_failure();
            } else {
                permit.complete_success();
            }

            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::sync::Notify;

    #[derive(Clone)]
    struct ControlledService {
        gate: Arc<Notify>,
    }

    impl Service<Request<Body>> for ControlledService {
        type Response = Response<Body>;
        type Error = std::convert::Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            let gate = Arc::clone(&self.gate);
            Box::pin(async move {
                gate.notified().await;
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap())
            })
        }
    }

    #[tokio::test]
    async fn half_open_pending_trial_blocks_concurrent_requests_even_after_reset_timeout() {
        tokio::time::pause();
        let breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            reset_timeout: Duration::from_millis(40),
        });

        breaker.on_failure();
        assert_eq!(breaker.state(), CircuitState::Open);

        tokio::time::advance(Duration::from_millis(50)).await;
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Even after HalfOpen itself has lasted longer than reset_timeout,
        // a pending trial must hold the slot until it completes or drops.
        tokio::time::advance(Duration::from_millis(50)).await;

        let gate = Arc::new(Notify::new());
        let mut svc = CircuitBreakerService::new(
            ControlledService {
                gate: Arc::clone(&gate),
            },
            breaker.clone(),
        );

        let mut trial_fut = tokio::spawn({
            let req = Request::builder().body(Body::empty()).unwrap();
            svc.call(req)
        });
        // Poll once so the trial acquires its HalfOpen permit and awaits the gate.
        tokio::task::yield_now().await;

        // A second request arriving after another reset_timeout must still be rejected.
        tokio::time::advance(Duration::from_millis(50)).await;
        let second_req = Request::builder().body(Body::empty()).unwrap();
        let second_resp = svc.call(second_req).await.unwrap();
        assert_eq!(second_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Dropping/aborting the pending trial releases the slot immediately.
        trial_fut.abort();
        let _ = (&mut trial_fut).await;

        // Now a new trial is admitted immediately without waiting for reset_timeout.
        gate.notify_one();
        let third_req = Request::builder().body(Body::empty()).unwrap();
        let third_resp = svc.call(third_req).await.unwrap();
        assert_eq!(third_resp.status(), StatusCode::OK);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn superseded_half_open_permit_completion_is_ignored() {
        let breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 1,
            reset_timeout: Duration::from_millis(0),
        });

        // Trip to Open and immediately transition to HalfOpen.
        breaker.on_failure();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Trial A acquires generation 1.
        let permit_a = breaker.try_acquire().expect("trial A should be admitted");
        assert!(permit_a.generation.is_some());

        // Simulate trial A being superseded by another cycle (e.g., failure -> Open -> HalfOpen -> trial B).
        breaker.on_failure();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        let permit_b = breaker.try_acquire().expect("trial B should be admitted");
        assert_ne!(permit_a.generation, permit_b.generation);

        // Late completion from superseded trial A must be ignored: it must NOT
        // close the circuit or clear trial B's in-flight slot.
        permit_a.complete_success();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
        assert!(
            breaker.try_acquire().is_none(),
            "trial B's slot must remain occupied after superseded trial A completes"
        );

        // Trial B's own completion closes the circuit.
        permit_b.complete_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }
}
