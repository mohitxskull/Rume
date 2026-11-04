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

/// Tower middleware layer that enforces that an authenticated user ([`crate::auth::identity::AuthUser`])
/// belongs to the resolved tenant ([`crate::tenant::model::Tenant`]) present in request extensions.
///
/// If an `AuthUser` has a `tenant_id` or `"tenant_id"` / `"organization_id"` metadata entry,
/// it must match the resolved `Tenant::id`. Mismatches are rejected with 403 Forbidden.
/// If `require_tenant_id` is true, an authenticated user without any tenant attribute is also rejected with 403 Forbidden.
#[derive(Clone, Debug, Default)]
pub struct TenantMembershipLayer {
    require_tenant_id: bool,
}

impl TenantMembershipLayer {
    /// Creates a new tenant membership layer that enforces tenant matching when a user has an assigned tenant ID.
    pub fn new() -> Self {
        Self {
            require_tenant_id: false,
        }
    }

    /// Creates a strict tenant membership layer that additionally rejects any user lacking a tenant association.
    pub fn strict() -> Self {
        Self {
            require_tenant_id: true,
        }
    }
}

impl<S> Layer<S> for TenantMembershipLayer {
    type Service = TenantMembershipService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TenantMembershipService {
            inner,
            require_tenant_id: self.require_tenant_id,
        }
    }
}

/// Tower service executing tenant authorization verification.
#[derive(Clone, Debug)]
pub struct TenantMembershipService<S> {
    inner: S,
    require_tenant_id: bool,
}

impl<S> Service<Request<Body>> for TenantMembershipService<S>
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
        let require_tenant_id = self.require_tenant_id;
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let tenant = req.extensions().get::<crate::tenant::model::Tenant>();
            let user = req.extensions().get::<crate::auth::identity::AuthUser>();

            if let (Some(tenant), Some(user)) = (tenant, user) {
                let user_tenant = user
                    .tenant_id
                    .as_deref()
                    .or_else(|| user.metadata.get("tenant_id").map(|s| s.as_str()))
                    .or_else(|| user.metadata.get("organization_id").map(|s| s.as_str()));

                match user_tenant {
                    Some(tid) if tid == tenant.id => {
                        // User belongs to resolved tenant, continue
                    }
                    Some(_) => {
                        // Tenant mismatch! Forbidden
                        tracing::warn!(
                            user_id = %user.id,
                            user_tenant = ?user_tenant,
                            resolved_tenant = %tenant.id,
                            "Tenant membership authorization rejected: user does not belong to requested tenant"
                        );
                        return Ok(AppError::Forbidden.into_response());
                    }
                    None => {
                        if require_tenant_id {
                            tracing::warn!(
                                user_id = %user.id,
                                "Tenant membership authorization rejected: user lacks required tenant association"
                            );
                            return Ok(AppError::Forbidden.into_response());
                        }
                    }
                }
            }

            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use crate::auth::identity::AuthUser;
    use crate::tenant::model::Tenant;

    async fn ok_handler() -> impl IntoResponse {
        StatusCode::OK
    }

    #[tokio::test]
    async fn test_tenant_membership_matching_tenant_allowed() {
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(TenantMembershipLayer::new());

        let mut req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(Tenant::new("acme", "acme"));
        req.extensions_mut().insert(AuthUser::new("user-1").with_tenant_id("acme"));

        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_tenant_membership_mismatched_tenant_forbidden() {
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(TenantMembershipLayer::new());

        let mut req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(Tenant::new("globex", "globex"));
        req.extensions_mut().insert(AuthUser::new("user-1").with_tenant_id("acme"));

        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_tenant_membership_strict_mode() {
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(TenantMembershipLayer::strict());

        // Without tenant association in AuthUser -> rejected under strict mode
        let mut req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(Tenant::new("acme", "acme"));
        req.extensions_mut().insert(AuthUser::new("user-no-tenant"));

        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }
}
