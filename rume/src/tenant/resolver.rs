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
// Why: Separate maps for ID and slug lookups prevent registration order from overwriting
// entries across namespaces when Tenant A's slug matches Tenant B's ID.
#[derive(Debug, Clone)]
pub struct HeaderTenantResolver {
    header_name: String,
    by_id: Arc<RwLock<BTreeMap<String, Tenant>>>,
    by_slug: Arc<RwLock<BTreeMap<String, Tenant>>>,
}

impl HeaderTenantResolver {
    /// Creates a new header resolver reading from `x-tenant-id`.
    pub fn new() -> Self {
        Self {
            header_name: "x-tenant-id".to_string(),
            by_id: Arc::new(RwLock::new(BTreeMap::new())),
            by_slug: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Sets a custom header name.
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header_name = header.into().to_lowercase();
        self
    }

    /// Attempts to register a known tenant by ID and slug, returning an error if a conflict exists.
    pub fn try_add_tenant(&self, tenant: Tenant) -> Result<(), TenantError> {
        let mut id_map = self
            .by_id
            .write()
            .map_err(|e| TenantError::ResolverError(e.to_string()))?;
        let mut slug_map = self
            .by_slug
            .write()
            .map_err(|e| TenantError::ResolverError(e.to_string()))?;

        if let Some(existing) = slug_map.get(&tenant.slug)
            && existing.id != tenant.id
        {
            return Err(TenantError::Conflict(format!(
                "tenant slug '{}' already registered to tenant '{}'",
                tenant.slug, existing.id
            )));
        }
        if let Some(existing) = id_map.get(&tenant.id)
            && existing.slug != tenant.slug
        {
            return Err(TenantError::Conflict(format!(
                "tenant ID '{}' already registered to tenant with slug '{}'",
                tenant.id, existing.slug
            )));
        }

        id_map.insert(tenant.id.clone(), tenant.clone());
        slug_map.insert(tenant.slug.clone(), tenant);
        Ok(())
    }

    /// Registers a known tenant by ID or slug.
    pub fn add_tenant(self, tenant: Tenant) -> Self {
        if let Err(e) = self.try_add_tenant(tenant) {
            tracing::warn!("Failed to add tenant: {e}");
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

            // Why: Look up by ID first, falling back to slug lookup only if not found in by_id,
            // ensuring primary tenant identifiers take precedence over vanity slugs.
            let id_lookup = {
                let id_map = self
                    .by_id
                    .read()
                    .map_err(|e| TenantError::ResolverError(e.to_string()))?;
                id_map.get(key).cloned()
            };

            if let Some(tenant) = id_lookup {
                return Ok(Some(tenant));
            }

            let slug_map = self
                .by_slug
                .read()
                .map_err(|e| TenantError::ResolverError(e.to_string()))?;

            if let Some(tenant) = slug_map.get(key) {
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

            // Convert host to lowercase and remove optional port: "ACME.myplatform.com:8080" -> "acme.myplatform.com", "[::1]:8080" -> "[::1]"
            let host_lower = host_str.to_ascii_lowercase();
            let host_no_port = if host_lower.starts_with('[') {
                if let Some(bracket_end) = host_lower.find(']') {
                    &host_lower[..=bracket_end]
                } else {
                    &host_lower
                }
            } else {
                host_lower.split(':').next().unwrap_or(&host_lower)
            };

            // Why: Normalizing base_domain to guarantee a leading dot (".{base_domain}") ensures that
            // host.strip_suffix matches strictly on a DNS label boundary. Without the leading dot,
            // "evilapp.platform.io" matches suffix "app.platform.io", extracting "evil" as a valid tenant.
            let normalized_base = if self.base_domain.starts_with('.') {
                self.base_domain.clone()
            } else {
                format!(".{}", self.base_domain)
            };

            if let Some(subdomain) = host_no_port.strip_suffix(&normalized_base) {
                // Why: A tenant subdomain must be a single DNS label. Subdomains containing dots
                // (e.g. "attacker.acme.app.platform.io") represent nested domain delegations that must
                // not be treated as a composite slug.
                if subdomain.is_empty()
                    || subdomain.contains('.')
                    || !subdomain
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    return Ok(None);
                }

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

            Ok(None)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[tokio::test]
    async fn subdomain_resolver_rejects_unanchored_domain_spoofing() {
        let evil_tenant = Tenant::new("evil-id", "evil");
        let resolver = SubdomainTenantResolver::new("app.platform.io").add_tenant(evil_tenant);

        let (parts, _) = Request::builder()
            .header(HOST, "evilapp.platform.io")
            .body(())
            .unwrap()
            .into_parts();

        let resolved = resolver
            .resolve(&parts)
            .await
            .expect("resolution should not error");
        assert_eq!(resolved, None);
    }

    #[tokio::test]
    async fn subdomain_resolver_rejects_multi_level_subdomains() {
        let attacker_tenant = Tenant::new("attacker-id", "attacker.acme");
        let resolver = SubdomainTenantResolver::new("app.platform.io").add_tenant(attacker_tenant);

        let (parts, _) = Request::builder()
            .header(HOST, "attacker.acme.app.platform.io")
            .body(())
            .unwrap()
            .into_parts();

        let resolved = resolver
            .resolve(&parts)
            .await
            .expect("resolution should not error");
        assert_eq!(resolved, None);
    }

    #[tokio::test]
    async fn subdomain_resolver_accepts_valid_single_label_subdomain() {
        let acme_tenant = Tenant::new("acme-id", "acme");
        let resolver =
            SubdomainTenantResolver::new("app.platform.io").add_tenant(acme_tenant.clone());

        let (parts, _) = Request::builder()
            .header(HOST, "acme.app.platform.io")
            .body(())
            .unwrap()
            .into_parts();

        let resolved = resolver
            .resolve(&parts)
            .await
            .expect("resolution should not error");
        assert_eq!(resolved, Some(acme_tenant));
    }

    #[tokio::test]
    async fn subdomain_resolver_handles_leading_dot_configuration_gracefully() {
        let acme_tenant = Tenant::new("acme-id", "acme");
        let resolver_dot =
            SubdomainTenantResolver::new(".app.platform.io").add_tenant(acme_tenant.clone());
        let resolver_nodot =
            SubdomainTenantResolver::new("app.platform.io").add_tenant(acme_tenant.clone());

        let (parts1, _) = Request::builder()
            .header(HOST, "acme.app.platform.io")
            .body(())
            .unwrap()
            .into_parts();

        let (parts2, _) = Request::builder()
            .header(HOST, "acme.app.platform.io")
            .body(())
            .unwrap()
            .into_parts();

        let res1 = resolver_dot.resolve(&parts1).await.expect("resolution ok");
        let res2 = resolver_nodot
            .resolve(&parts2)
            .await
            .expect("resolution ok");

        assert_eq!(res1, Some(acme_tenant.clone()));
        assert_eq!(res2, Some(acme_tenant));
    }

    #[tokio::test]
    async fn header_resolver_prevents_slug_overwrite_by_colliding_id() {
        let tenant1 = Tenant::new("t-001", "acme");
        let tenant2 = Tenant::new("acme", "globex");

        let resolver = HeaderTenantResolver::new()
            .add_tenant(tenant1.clone())
            .add_tenant(tenant2.clone());

        // Querying "acme" resolves Tenant 2 by ID
        let (parts_acme, _) = Request::builder()
            .header("x-tenant-id", "acme")
            .body(())
            .unwrap()
            .into_parts();
        let res_acme = resolver.resolve(&parts_acme).await.expect("resolution ok");
        assert_eq!(res_acme, Some(tenant2.clone()));

        // Querying "t-001" resolves Tenant 1 by ID
        let (parts_t1, _) = Request::builder()
            .header("x-tenant-id", "t-001")
            .body(())
            .unwrap()
            .into_parts();
        let res_t1 = resolver.resolve(&parts_t1).await.expect("resolution ok");
        assert_eq!(res_t1, Some(tenant1.clone()));

        // Reversed registration order: Tenant 2 registered before Tenant 1.
        // Adding Tenant 1 with slug "acme" must not overwrite Tenant 2 whose ID is "acme".
        let resolver_rev = HeaderTenantResolver::new()
            .add_tenant(tenant2.clone())
            .add_tenant(tenant1.clone());

        let (parts_acme_rev, _) = Request::builder()
            .header("x-tenant-id", "acme")
            .body(())
            .unwrap()
            .into_parts();
        let res_acme_rev = resolver_rev
            .resolve(&parts_acme_rev)
            .await
            .expect("resolution ok");
        assert_eq!(res_acme_rev, Some(tenant2));

        let (parts_t1_rev, _) = Request::builder()
            .header("x-tenant-id", "t-001")
            .body(())
            .unwrap()
            .into_parts();
        let res_t1_rev = resolver_rev
            .resolve(&parts_t1_rev)
            .await
            .expect("resolution ok");
        assert_eq!(res_t1_rev, Some(tenant1));
    }

    #[tokio::test]
    async fn header_resolver_slug_fallback_resolves_when_no_id_conflict() {
        let tenant1 = Tenant::new("t-001", "acme");
        let resolver = HeaderTenantResolver::new().add_tenant(tenant1.clone());

        let (parts, _) = Request::builder()
            .header("x-tenant-id", "acme")
            .body(())
            .unwrap()
            .into_parts();

        let res = resolver.resolve(&parts).await.expect("resolution ok");
        assert_eq!(res, Some(tenant1));
    }
}
