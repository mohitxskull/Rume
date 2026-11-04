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

/// Tier classification for a tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum TenantTier {
    Free,
    Pro,
    Enterprise,
    Custom,
}

/// Tiered quota configuration specifying resource limits per tenant tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TieredQuotaPolicy {
    /// Limit for Free tier (default: 100).
    pub free_limit: usize,
    /// Limit for Pro tier (default: 1,000).
    pub pro_limit: usize,
    /// Limit for Enterprise tier (default: 10,000).
    pub enterprise_limit: usize,
}

impl Default for TieredQuotaPolicy {
    fn default() -> Self {
        Self {
            free_limit: 100,
            pro_limit: 1000,
            enterprise_limit: 10000,
        }
    }
}

impl TieredQuotaPolicy {
    /// Creates a custom tiered quota policy.
    pub fn new(free_limit: usize, pro_limit: usize, enterprise_limit: usize) -> Self {
        Self {
            free_limit,
            pro_limit,
            enterprise_limit,
        }
    }

    /// Resolves the allowed quota limit for a given [`TenantTier`].
    pub fn limit_for_tier(&self, tier: TenantTier) -> usize {
        match tier {
            TenantTier::Free => self.free_limit,
            TenantTier::Pro => self.pro_limit,
            TenantTier::Enterprise => self.enterprise_limit,
            TenantTier::Custom => self.free_limit,
        }
    }

    /// Resolves the tier and corresponding quota limit for a [`crate::tenant::model::Tenant`].
    ///
    /// Respects `tenant.metadata["quota_limit"]` if explicitly configured,
    /// otherwise infers the tier from `tenant.metadata["tier"]` (defaults to `Free`).
    pub fn limit_for_tenant(&self, tenant: &crate::tenant::model::Tenant) -> (TenantTier, usize) {
        if let Some(custom) = tenant
            .metadata
            .get("quota_limit")
            .and_then(|s| s.parse::<usize>().ok())
        {
            return (TenantTier::Custom, custom);
        }

        let tier = match tenant
            .metadata
            .get("tier")
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("pro") => TenantTier::Pro,
            Some("enterprise") => TenantTier::Enterprise,
            _ => TenantTier::Free,
        };

        let limit = self.limit_for_tier(tier);
        (tier, limit)
    }
}

/// Tower middleware layer that enforces tiered quota resource limits for resolved tenants.
#[derive(Clone)]
pub struct TenantQuotaLayer {
    store: Arc<dyn crate::tenant::quota::TenantQuotaStore>,
    policy: TieredQuotaPolicy,
}

impl TenantQuotaLayer {
    /// Creates a new tenant quota layer with the given store and default tiered policy.
    pub fn new(store: Arc<dyn crate::tenant::quota::TenantQuotaStore>) -> Self {
        Self {
            store,
            policy: TieredQuotaPolicy::default(),
        }
    }

    /// Configures a custom tiered quota policy.
    pub fn with_policy(mut self, policy: TieredQuotaPolicy) -> Self {
        self.policy = policy;
        self
    }
}

impl<S> Layer<S> for TenantQuotaLayer {
    type Service = TenantQuotaService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TenantQuotaService {
            inner,
            store: Arc::clone(&self.store),
            policy: self.policy.clone(),
        }
    }
}

/// Tower service executing tiered tenant quota enforcement.
#[derive(Clone)]
pub struct TenantQuotaService<S> {
    inner: S,
    store: Arc<dyn crate::tenant::quota::TenantQuotaStore>,
    policy: TieredQuotaPolicy,
}

impl<S> Service<Request<Body>> for TenantQuotaService<S>
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
        let store = Arc::clone(&self.store);
        let policy = self.policy.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let tenant = req
                .extensions()
                .get::<crate::tenant::model::Tenant>()
                .cloned()
                .or_else(|| {
                    req.extensions()
                        .get::<crate::tenant::model::TenantContext>()
                        .map(|tc| tc.0.clone())
                });

            if let Some(tenant) = tenant {
                let (tier, limit) = policy.limit_for_tenant(&tenant);
                match store.acquire(&tenant.id, limit).await {
                    Ok(true) => {
                        let mut response = inner.call(req).await?;
                        let headers = response.headers_mut();
                        if let Ok(val) = axum::http::HeaderValue::from_str(&limit.to_string()) {
                            headers.insert(
                                axum::http::header::HeaderName::from_static("x-quota-limit"),
                                val,
                            );
                        }
                        let tier_str = match tier {
                            TenantTier::Free => "free",
                            TenantTier::Pro => "pro",
                            TenantTier::Enterprise => "enterprise",
                            TenantTier::Custom => "custom",
                        };
                        let val = axum::http::HeaderValue::from_static(tier_str);
                        headers.insert(
                            axum::http::header::HeaderName::from_static("x-quota-tier"),
                            val,
                        );
                        Ok(response)
                    }
                    Ok(false) => {
                        let app_err: AppError = TenantError::QuotaExceeded {
                            tenant_id: tenant.id.clone(),
                            limit,
                        }
                        .into();
                        let mut response = app_err.into_response();
                        let val = axum::http::HeaderValue::from_static("60");
                        response
                            .headers_mut()
                            .insert(axum::http::header::RETRY_AFTER, val);
                        Ok(response)
                    }
                    Err(err) => {
                        tracing::error!(tenant_id = %tenant.id, error = %err, "Tenant quota store error");
                        let app_err = AppError::Internal {
                            source: crate::error::InternalSource::new(format!("Quota verification error: {err}")),
                            request_id: None,
                        };
                        Ok(app_err.into_response())
                    }
                }
            } else {
                inner.call(req).await
            }
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

    #[tokio::test]
    async fn test_tenant_quota_layer_enforces_tier_limits() {
        let store = Arc::new(crate::tenant::quota::InMemoryTenantQuotaStore::new());
        let policy = TieredQuotaPolicy {
            free_limit: 2,
            pro_limit: 10,
            enterprise_limit: 100,
        };

        let app = Router::new()
            .route("/api/data", get(ok_handler))
            .layer(TenantQuotaLayer::new(store).with_policy(policy));

        // 1. Free tenant makes 2 requests -> both succeed with headers
        for _ in 0..2 {
            let mut req = Request::builder()
                .uri("/api/data")
                .body(Body::empty())
                .unwrap();
            let tenant = Tenant::new("tenant_free", "tenant-free");
            req.extensions_mut().insert(tenant);

            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(res.headers().get("x-quota-limit").unwrap(), "2");
            assert_eq!(res.headers().get("x-quota-tier").unwrap(), "free");
        }

        // 2. 3rd request for free tenant -> rejected with 429 Too Many Requests
        let mut req_overflow = Request::builder()
            .uri("/api/data")
            .body(Body::empty())
            .unwrap();
        req_overflow
            .extensions_mut()
            .insert(Tenant::new("tenant_free", "tenant-free"));

        let res_overflow = app.clone().oneshot(req_overflow).await.unwrap();
        assert_eq!(res_overflow.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res_overflow.headers().get(axum::http::header::RETRY_AFTER).unwrap(), "60");

        // 3. Pro tenant can still make requests up to pro_limit
        let mut req_pro = Request::builder()
            .uri("/api/data")
            .body(Body::empty())
            .unwrap();
        let mut pro_tenant = Tenant::new("tenant_pro", "tenant-pro");
        pro_tenant.metadata.insert("tier".into(), "pro".into());
        req_pro.extensions_mut().insert(pro_tenant);

        let res_pro = app.clone().oneshot(req_pro).await.unwrap();
        assert_eq!(res_pro.status(), StatusCode::OK);
        assert_eq!(res_pro.headers().get("x-quota-limit").unwrap(), "10");
        assert_eq!(res_pro.headers().get("x-quota-tier").unwrap(), "pro");
    }
}
