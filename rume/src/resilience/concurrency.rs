//! Concurrency limiting and load-shedding middleware.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{Request, Response};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::resilience::error::ResilienceError;

/// Concurrency limiter restricting in-flight requests and shedding excess load.
#[derive(Debug, Clone)]
pub struct ConcurrencyLimiter {
    max: usize,
    current: Arc<AtomicUsize>,
}

impl ConcurrencyLimiter {
    /// Creates a new concurrency limiter with the specified maximum in-flight requests.
    pub fn new(max: usize) -> Self {
        Self {
            max,
            current: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Attempts to acquire an execution permit.
    pub fn acquire(&self) -> Result<ConcurrencyPermit, ResilienceError> {
        let current = self.current.fetch_add(1, Ordering::SeqCst);
        if current >= self.max {
            self.current.fetch_sub(1, Ordering::SeqCst);
            return Err(ResilienceError::LoadShed {
                current,
                max: self.max,
            });
        }

        Ok(ConcurrencyPermit {
            counter: Arc::clone(&self.current),
        })
    }

    /// Returns the current number of in-flight requests.
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }
}

/// Active execution permit that decrements the concurrency counter when dropped.
#[derive(Debug)]
pub struct ConcurrencyPermit {
    counter: Arc<AtomicUsize>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Tower layer for concurrency limiting.
#[derive(Clone)]
pub struct ConcurrencyLimitLayer {
    limiter: ConcurrencyLimiter,
}

impl ConcurrencyLimitLayer {
    /// Creates a new concurrency limit layer.
    pub fn new(max: usize) -> Self {
        Self {
            limiter: ConcurrencyLimiter::new(max),
        }
    }
}

impl<S> Layer<S> for ConcurrencyLimitLayer {
    type Service = ConcurrencyLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConcurrencyLimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

/// Tower service for concurrency limiting.
#[derive(Clone)]
pub struct ConcurrencyLimitService<S> {
    inner: S,
    limiter: ConcurrencyLimiter,
}

impl<S> Service<Request<Body>> for ConcurrencyLimitService<S>
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

        Box::pin(async move {
            let _permit = match limiter.acquire() {
                Ok(permit) => permit,
                Err(err) => {
                    let app_err: AppError = err.into();
                    return Ok(app_err.into_response());
                }
            };

            inner.call(req).await
        })
    }
}
