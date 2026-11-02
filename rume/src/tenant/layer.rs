//! Tower middleware layer for resolving and injecting tenant contexts.

use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{Request, Response};
use axum::response::IntoResponse;
use tower::{Layer, Service};

use crate::error::AppError;
use crate::tenant::error::TenantError;
use crate::tenant::resolver::TenantResolver;

/// Tower layer for tenant resolution.
#[derive(Clone)]
pub struct TenantLayer {
    resolver: Arc<dyn TenantResolver>,
    required: bool,
}

impl TenantLayer {
    /// Creates a required tenant layer; missing or invalid tenants are rejected immediately.
    pub fn required(resolver: impl TenantResolver) -> Self {
        Self {
            resolver: Arc::new(resolver),
            required: true,
        }
    }

    /// Creates an optional tenant layer; requests without a tenant pass through cleanly.
    pub fn optional(resolver: impl TenantResolver) -> Self {
        Self {
            resolver: Arc::new(resolver),
            required: false,
        }
    }
}

impl<S> Layer<S> for TenantLayer {
    type Service = TenantService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TenantService {
            inner,
            resolver: Arc::clone(&self.resolver),
            required: self.required,
        }
    }
}

/// Tower service executing tenant resolution.
#[derive(Clone)]
pub struct TenantService<S> {
    inner: S,
    resolver: Arc<dyn TenantResolver>,
    required: bool,
}

impl<S> Service<Request<Body>> for TenantService<S>
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
        let resolver = Arc::clone(&self.resolver);
        let required = self.required;
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let (parts, body) = req.into_parts();

            match resolver.resolve(&parts).await {
                Ok(Some(tenant)) => {
                    let mut req = Request::from_parts(parts, body);
                    req.extensions_mut().insert(tenant);
                    inner.call(req).await
                }
                Ok(None) => {
                    if required {
                        let err: AppError = TenantError::MissingTenant.into();
                        return Ok(err.into_response());
                    }
                    let req = Request::from_parts(parts, body);
                    inner.call(req).await
                }
                Err(err) => {
                    let app_err: AppError = err.into();
                    Ok(app_err.into_response())
                }
            }
        })
    }
}
