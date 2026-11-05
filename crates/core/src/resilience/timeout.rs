//! Adaptive request timeout middleware.

use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, Response};
use axum::response::IntoResponse;
use tokio::time::timeout;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::resilience::error::ResilienceError;

/// Tower layer enforcing maximum execution duration on HTTP requests.
#[derive(Debug, Clone)]
pub struct TimeoutLayer {
    duration: Duration,
}

impl TimeoutLayer {
    /// Creates a new timeout layer with the specified duration.
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }
}

impl<S> Layer<S> for TimeoutLayer {
    type Service = TimeoutService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TimeoutService {
            inner,
            duration: self.duration,
        }
    }
}

/// Tower service wrapping inner handler with a timeout deadline.
#[derive(Clone)]
pub struct TimeoutService<S> {
    inner: S,
    duration: Duration,
}

impl<S> Service<Request<Body>> for TimeoutService<S>
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
        let duration = self.duration;
        let mut inner = self.inner.clone();

        Box::pin(async move {
            match timeout(duration, inner.call(req)).await {
                Ok(result) => result,
                Err(_) => {
                    let err: AppError = ResilienceError::Timeout.into();
                    Ok(err.into_response())
                }
            }
        })
    }
}
