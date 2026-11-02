//! Tenant resolution strategies (headers, subdomains, path prefixes).

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use axum::http::header::HOST;
use axum::http::request::Parts;

use crate::tenant::error::TenantError;
use crate::tenant::model::Tenant;

/// Pinned boxed future type alias for async tenant resolution.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait implemented by tenant resolvers.
pub trait TenantResolver: Debug + Send + Sync + 'static {
    /// Resolves a tenant from incoming request parts.
    fn resolve<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>>;
}

/// Resolves tenant from a configured request header (default `X-Tenant-ID`).
#[derive(Debug, Clone)]
pub struct HeaderTenantResolver {
    header_name: String,
    tenants: Arc<RwLock<BTreeMap<String, Tenant>>>,
}

impl HeaderTenantResolver {
    /// Creates a new header resolver reading from `x-tenant-id`.
    pub fn new() -> Self {
        Self {
            header_name: "x-tenant-id".to_string(),
            tenants: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Sets a custom header name.
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header_name = header.into().to_lowercase();
        self
    }

    /// Registers a known tenant by ID or slug.
    pub fn add_tenant(self, tenant: Tenant) -> Self {
        if let Ok(mut map) = self.tenants.write() {
            map.insert(tenant.id.clone(), tenant.clone());
            map.insert(tenant.slug.clone(), tenant);
        }
        self
    }
}

impl Default for HeaderTenantResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl TenantResolver for HeaderTenantResolver {
    fn resolve<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>> {
        Box::pin(async move {
            let Some(header_val) = parts.headers.get(&self.header_name) else {
                return Ok(None);
            };

            let Ok(key) = header_val.to_str() else {
                return Err(TenantError::ResolverError("malformed tenant header".into()));
            };

            let map = self
                .tenants
                .read()
                .map_err(|e| TenantError::ResolverError(e.to_string()))?;

            if let Some(tenant) = map.get(key) {
                Ok(Some(tenant.clone()))
            } else {
                Err(TenantError::TenantNotFound(key.to_string()))
            }
        })
    }
}

/// Resolves tenant from the incoming `Host` subdomain (e.g. `acme.myplatform.com` -> `acme`).
#[derive(Debug, Clone)]
pub struct SubdomainTenantResolver {
    base_domain: String,
    tenants: Arc<RwLock<BTreeMap<String, Tenant>>>,
}

impl SubdomainTenantResolver {
    /// Creates a new subdomain resolver for the given base domain.
    pub fn new(base_domain: impl Into<String>) -> Self {
        Self {
            base_domain: base_domain.into(),
            tenants: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Registers a tenant associated with a subdomain slug.
    pub fn add_tenant(self, tenant: Tenant) -> Self {
        if let Ok(mut map) = self.tenants.write() {
            map.insert(tenant.slug.clone(), tenant);
        }
        self
    }
}

impl TenantResolver for SubdomainTenantResolver {
    fn resolve<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>> {
        Box::pin(async move {
            let Some(host_val) = parts.headers.get(HOST) else {
                return Ok(None);
            };

            let Ok(host_str) = host_val.to_str() else {
                return Err(TenantError::ResolverError("malformed host header".into()));
            };

            // Remove optional port: "acme.myplatform.com:8080" -> "acme.myplatform.com"
            let host_no_port = host_str.split(':').next().unwrap_or(host_str);

            if let Some(subdomain) = host_no_port.strip_suffix(&self.base_domain) {
                let subdomain = subdomain.trim_end_matches('.');
                if !subdomain.is_empty() {
                    let map = self
                        .tenants
                        .read()
                        .map_err(|e| TenantError::ResolverError(e.to_string()))?;

                    if let Some(tenant) = map.get(subdomain) {
                        return Ok(Some(tenant.clone()));
                    } else {
                        return Err(TenantError::TenantNotFound(subdomain.to_string()));
                    }
                }
            }

            Ok(None)
        })
    }
}
