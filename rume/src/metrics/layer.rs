//! Tower middleware for recording HTTP latency, status, and in-flight counts into Prometheus metrics.

use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, Response};
use tower::{Layer, Service};

use crate::metrics::registry::MetricsRegistry;

/// Tower layer for recording HTTP request metrics.
#[derive(Debug, Clone)]
pub struct MetricsLayer {
    registry: MetricsRegistry,
}

impl MetricsLayer {
    /// Creates a new metrics layer wrapping the given registry.
    pub fn new(registry: MetricsRegistry) -> Self {
        Self { registry }
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            registry: self.registry.clone(),
        }
    }
}

struct InFlightGuard(MetricsRegistry);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.dec_in_flight();
    }
}

/// Tower service recording execution metrics.
#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    registry: MetricsRegistry,
}

impl<S> Service<Request<Body>> for MetricsService<S>
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
        if req.uri().path() == "/metrics" {
            let mut inner = self.inner.clone();
            return Box::pin(async move { inner.call(req).await });
        }

        let registry = self.registry.clone();
        let mut inner = self.inner.clone();
        let method = req.method().to_string();
        let start = Instant::now();

        registry.inc_in_flight();
        let guard = InFlightGuard(registry.clone());

        Box::pin(async move {
            let _guard = guard;
            let response = inner.call(req).await;
            let duration = start.elapsed();

            match response {
                Ok(res) => {
                    let status = res.status().as_u16();
                    registry.record_http_request(&method, status, duration);
                    Ok(res)
                }
                Err(err) => {
                    registry.record_http_request(&method, 500, duration);
                    Err(err)
                }
            }
        })
    }
}
