//! Tenant resolution strategies (headers and subdomains).

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use axum::http::header::HOST;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

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
            .map_err(|e| TenantError::Internal(e.to_string()))?;
        let mut slug_map = self
            .by_slug
            .write()
            .map_err(|e| TenantError::Internal(e.to_string()))?;

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

        // Cross-namespace collision checks (prevent shadowing):
        if let Some(existing) = id_map.get(&tenant.slug)
            && existing.id != tenant.id
        {
            return Err(TenantError::Conflict(format!(
                "tenant slug '{}' conflicts with existing tenant ID for '{}'",
                tenant.slug, existing.id
            )));
        }
        if let Some(existing) = slug_map.get(&tenant.id)
            && existing.id != tenant.id
        {
            return Err(TenantError::Conflict(format!(
                "tenant ID '{}' conflicts with existing tenant slug for '{}'",
                tenant.id, existing.id
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
            let Some(key) = extract_header_key(parts, &self.header_name)? else {
                return Ok(None);
            };

            // Why: Look up by ID first, falling back to slug lookup only if not found in by_id,
            // ensuring primary tenant identifiers take precedence over vanity slugs.
            let id_lookup = {
                let id_map = self
                    .by_id
                    .read()
                    .map_err(|e| TenantError::Internal(e.to_string()))?;
                id_map.get(key).cloned()
            };

            if let Some(tenant) = id_lookup {
                return Ok(Some(tenant));
            }

            let slug_map = self
                .by_slug
                .read()
                .map_err(|e| TenantError::Internal(e.to_string()))?;

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
            base_domain: normalize_base_domain(&base_domain.into()),
            tenants: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Attempts to register a tenant associated with a subdomain slug, returning an error on conflict.
    pub fn try_add_tenant(&self, tenant: Tenant) -> Result<(), TenantError> {
        let mut map = self
            .tenants
            .write()
            .map_err(|e| TenantError::Internal(e.to_string()))?;
        if let Some(existing) = map.get(&tenant.slug)
            && existing.id != tenant.id
        {
            return Err(TenantError::Conflict(format!(
                "subdomain slug '{}' already registered to tenant '{}'",
                tenant.slug, existing.id
            )));
        }
        map.insert(tenant.slug.clone(), tenant);
        Ok(())
    }

    /// Registers a tenant associated with a subdomain slug.
    pub fn add_tenant(self, tenant: Tenant) -> Self {
        if let Err(e) = self.try_add_tenant(tenant) {
            tracing::error!("Failed to add tenant: {e}");
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
            let Some(subdomain) = extract_subdomain_slug(parts, &self.base_domain)? else {
                return Ok(None);
            };

            let map = self
                .tenants
                .read()
                .map_err(|e| TenantError::Internal(e.to_string()))?;

            if let Some(tenant) = map.get(&subdomain) {
                Ok(Some(tenant.clone()))
            } else {
                Err(TenantError::TenantNotFound(subdomain))
            }
        })
    }
}

/// Async storage interface for persisted tenant records.
pub trait TenantStore: Debug + Send + Sync + 'static {
    /// Finds an active tenant by ID.
    fn find_by_id<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>>;

    /// Finds an active tenant by slug.
    fn find_by_slug<'a>(
        &'a self,
        slug: &'a str,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>>;

    /// Inserts or updates a tenant record.
    fn upsert<'a>(&'a self, tenant: &'a Tenant) -> BoxFuture<'a, Result<(), TenantError>>;

    /// Deactivates a tenant by ID.
    fn deactivate<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, TenantError>>;
}

/// Maximum accepted byte length for a tenant header identifier.
const MAX_TENANT_HEADER_LEN: usize = 128;

/// Extracts a tenant key from a request header, rejecting duplicate, non-UTF-8,
/// oversized, or invalid-charset values before querying any store or cache.
fn extract_header_key<'a>(
    parts: &'a Parts,
    header_name: &str,
) -> Result<Option<&'a str>, TenantError> {
    let mut values = parts.headers.get_all(header_name).iter();
    let Some(header_val) = values.next() else {
        return Ok(None);
    };
    // Why: duplicate tenant headers create proxy/backend ambiguity (`RC-17`); fail closed.
    if values.next().is_some() {
        return Err(TenantError::InvalidIdentifier(
            "duplicate tenant header".into(),
        ));
    }
    let Ok(key) = header_val.to_str() else {
        return Err(TenantError::InvalidIdentifier(
            "malformed tenant header".into(),
        ));
    };
    if key.is_empty()
        || key.len() > MAX_TENANT_HEADER_LEN
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(TenantError::InvalidIdentifier(
            "invalid tenant header identifier".into(),
        ));
    }
    Ok(Some(key))
}

/// Normalizes a base domain for subdomain extraction (lowercase, trimmed, no trailing dot).
fn normalize_base_domain(base_domain: &str) -> String {
    base_domain
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Extracts a single-label subdomain slug from `Host` or HTTP/2 `:authority`.
fn extract_subdomain_slug(parts: &Parts, base_domain: &str) -> Result<Option<String>, TenantError> {
    // Why: HTTP/1.1 sends the `Host` header, while HTTP/2 requests carry
    // the target authority in `:authority` (`parts.uri.authority()`).
    // Reject duplicate `Host` headers to prevent proxy/backend routing ambiguity (`RC-17`).
    let mut host_values = parts.headers.get_all(HOST).iter();
    let raw_host = if let Some(host_val) = host_values.next() {
        if host_values.next().is_some() {
            return Err(TenantError::InvalidIdentifier(
                "duplicate host header".into(),
            ));
        }
        let Ok(host_str) = host_val.to_str() else {
            return Err(TenantError::InvalidIdentifier(
                "malformed host header".into(),
            ));
        };
        host_str
    } else if let Some(authority) = parts.uri.authority() {
        authority.as_str()
    } else {
        return Ok(None);
    };

    // Convert host to lowercase, remove optional port, and strip trailing FQDN dot:
    // "ACME.myplatform.com.:8080" -> "acme.myplatform.com"
    let host_lower = raw_host.to_ascii_lowercase();
    let host_no_port = if host_lower.starts_with('[') {
        if let Some(bracket_end) = host_lower.find(']') {
            &host_lower[1..bracket_end]
        } else {
            &host_lower
        }
    } else {
        host_lower.split(':').next().unwrap_or(&host_lower)
    };
    let host_normalized = host_no_port.trim_end_matches('.');

    // Why: Normalizing base_domain to guarantee a leading dot (".{base_domain}") ensures that
    // host.strip_suffix matches strictly on a DNS label boundary. Without the leading dot,
    // "evilapp.platform.io" matches suffix "app.platform.io", extracting "evil" as a valid tenant.
    let normalized_base = if base_domain.starts_with('.') {
        base_domain.to_string()
    } else {
        format!(".{base_domain}")
    };

    if let Some(subdomain) = host_normalized.strip_suffix(&normalized_base) {
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
        return Ok(Some(subdomain.to_string()));
    }

    Ok(None)
}

/// Normalizes a URL path prefix to always begin and end with `/`.
fn normalize_path_prefix(prefix: &str) -> String {
    let mut normalized = prefix.trim().to_string();
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    if !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized
}

/// Validates a tenant slug extracted from a URL path segment.
fn validate_path_slug(slug: &str) -> Result<(), TenantError> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(TenantError::InvalidIdentifier(format!(
            "invalid tenant slug in path: '{slug}'"
        )));
    }
    Ok(())
}

/// Extracts and validates a tenant slug from a URL path matching `normalized_prefix`.
fn extract_path_prefix_slug(
    parts: &Parts,
    normalized_prefix: &str,
) -> Result<Option<String>, TenantError> {
    let Some(rest) = parts.uri.path().strip_prefix(normalized_prefix) else {
        return Ok(None);
    };
    let slug = rest.split('/').next().unwrap_or("");
    validate_path_slug(slug)?;
    Ok(Some(slug.to_string()))
}

/// SQL-backed [`TenantStore`] using `rume-data` queries.
#[derive(Debug, Clone)]
pub struct SqlTenantStore<B: rume_data::persistence::DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: rume_data::persistence::DatabaseBackend> SqlTenantStore<B> {
    /// Creates a new [`SqlTenantStore`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    fn row_to_tenant(row: rume_data::persistence::DbRow) -> Result<Tenant, TenantError> {
        let id: String = row
            .get("id")
            .map_err(|e| TenantError::Internal(format!("failed to decode tenant id: {e}")))?;
        let slug: String = row
            .get("slug")
            .map_err(|e| TenantError::Internal(format!("failed to decode tenant slug: {e}")))?;
        let metadata_json: Option<String> = row
            .get_opt("metadata_json")
            .map_err(|e| TenantError::Internal(format!("failed to decode metadata_json: {e}")))?;
        let metadata = match metadata_json.as_deref() {
            Some(raw) if !raw.is_empty() => serde_json::from_str(raw).map_err(|e| {
                TenantError::Internal(format!("failed to parse tenant metadata JSON: {e}"))
            })?,
            _ => BTreeMap::new(),
        };
        Ok(Tenant { id, slug, metadata })
    }
}

impl<B: rume_data::persistence::DatabaseBackend> TenantStore for SqlTenantStore<B> {
    fn find_by_id<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>> {
        Box::pin(async move {
            let sql = match self.backend.dialect() {
                "postgres" => rume_data::sql::PG_AUTH_TENANT_FIND_BY_ID,
                "mysql" => rume_data::sql::MYSQL_AUTH_TENANT_FIND_BY_ID,
                _ => rume_data::sql::SQLITE_AUTH_TENANT_FIND_BY_ID,
            };
            let row = self
                .backend
                .fetch_optional(sql, &[rume_data::persistence::DbValue::from(id)])
                .await
                .map_err(|e| TenantError::Internal(e.to_string()))?;
            row.map(Self::row_to_tenant).transpose()
        })
    }

    fn find_by_slug<'a>(
        &'a self,
        slug: &'a str,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>> {
        Box::pin(async move {
            let sql = match self.backend.dialect() {
                "postgres" => rume_data::sql::PG_AUTH_TENANT_FIND_BY_SLUG,
                "mysql" => rume_data::sql::MYSQL_AUTH_TENANT_FIND_BY_SLUG,
                _ => rume_data::sql::SQLITE_AUTH_TENANT_FIND_BY_SLUG,
            };
            let row = self
                .backend
                .fetch_optional(sql, &[rume_data::persistence::DbValue::from(slug)])
                .await
                .map_err(|e| TenantError::Internal(e.to_string()))?;
            row.map(Self::row_to_tenant).transpose()
        })
    }

    fn upsert<'a>(&'a self, tenant: &'a Tenant) -> BoxFuture<'a, Result<(), TenantError>> {
        Box::pin(async move {
            let sql = match self.backend.dialect() {
                "postgres" => rume_data::sql::PG_AUTH_TENANT_UPSERT,
                "mysql" => rume_data::sql::MYSQL_AUTH_TENANT_UPSERT,
                _ => rume_data::sql::SQLITE_AUTH_TENANT_UPSERT,
            };
            let metadata_json = serde_json::to_string(&tenant.metadata)
                .map_err(|e| TenantError::Internal(format!("failed to serialize metadata: {e}")))?;
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let params = [
                rume_data::persistence::DbValue::from(tenant.id.as_str()),
                rume_data::persistence::DbValue::from(tenant.slug.as_str()),
                rume_data::persistence::DbValue::from(metadata_json),
                rume_data::persistence::DbValue::from(created_at),
            ];
            self.backend
                .execute_params(sql, &params)
                .await
                .map_err(|e| TenantError::Internal(e.to_string()))?;
            Ok(())
        })
    }

    fn deactivate<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, TenantError>> {
        Box::pin(async move {
            let sql = match self.backend.dialect() {
                "postgres" => rume_data::sql::PG_AUTH_TENANT_DEACTIVATE,
                "mysql" => rume_data::sql::MYSQL_AUTH_TENANT_DEACTIVATE,
                _ => rume_data::sql::SQLITE_AUTH_TENANT_DEACTIVATE,
            };
            let affected = self
                .backend
                .execute_params(sql, &[rume_data::persistence::DbValue::from(id)])
                .await
                .map_err(|e| TenantError::Internal(e.to_string()))?;
            Ok(affected > 0)
        })
    }
}

/// Resolves tenant from a URL path prefix (e.g. `/t/{slug}/...`).
#[derive(Debug, Clone)]
pub struct PathPrefixTenantResolver {
    prefix: String,
    tenants: Arc<RwLock<BTreeMap<String, Tenant>>>,
    store: Option<Arc<dyn TenantStore>>,
}

impl PathPrefixTenantResolver {
    /// Creates a new path-prefix resolver with default prefix `"/t/"`.
    pub fn new() -> Self {
        Self {
            prefix: "/t/".to_string(),
            tenants: Arc::new(RwLock::new(BTreeMap::new())),
            store: None,
        }
    }

    /// Sets a custom path prefix.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = normalize_path_prefix(&prefix.into());
        self
    }

    /// Backs this resolver with a [`TenantStore`].
    pub fn with_store<S: TenantStore>(mut self, store: Arc<S>) -> Self {
        self.store = Some(store);
        self
    }

    /// Attempts to register a tenant by slug, returning an error on conflict.
    pub fn try_add_tenant(&self, tenant: Tenant) -> Result<(), TenantError> {
        validate_path_slug(&tenant.slug)?;
        let mut map = self
            .tenants
            .write()
            .map_err(|e| TenantError::Internal(e.to_string()))?;
        if let Some(existing) = map.get(&tenant.slug)
            && existing.id != tenant.id
        {
            return Err(TenantError::Conflict(format!(
                "path-prefix slug '{}' already registered to tenant '{}'",
                tenant.slug, existing.id
            )));
        }
        map.insert(tenant.slug.clone(), tenant);
        Ok(())
    }

    /// Registers a tenant by slug.
    pub fn add_tenant(self, tenant: Tenant) -> Self {
        if let Err(e) = self.try_add_tenant(tenant) {
            tracing::warn!("Failed to add tenant: {e}");
        }
        self
    }
}

impl Default for PathPrefixTenantResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl TenantResolver for PathPrefixTenantResolver {
    fn resolve<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>> {
        Box::pin(async move {
            let Some(slug) = extract_path_prefix_slug(parts, &self.prefix)? else {
                return Ok(None);
            };

            let static_hit = {
                let map = self
                    .tenants
                    .read()
                    .map_err(|e| TenantError::Internal(e.to_string()))?;
                map.get(&slug).cloned()
            };
            if let Some(tenant) = static_hit {
                return Ok(Some(tenant));
            }

            if let Some(store) = &self.store
                && let Some(tenant) = store.find_by_slug(&slug).await?
            {
                return Ok(Some(tenant));
            }

            Err(TenantError::TenantNotFound(slug))
        })
    }
}

/// Strategy used by [`CachedDbTenantResolver`] to extract a tenant key from requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantKeyExtractor {
    /// Extract tenant ID or slug from a request header.
    Header(String),
    /// Extract tenant slug from a subdomain of the given base domain.
    Subdomain(String),
    /// Extract tenant slug from a URL path prefix.
    PathPrefix(String),
}

#[derive(Debug, Serialize, Deserialize)]
enum CachedTenantEntry {
    Found(Tenant),
    NotFound,
}

/// DB-backed [`TenantResolver`] with positive/negative caching and explicit invalidation.
#[derive(Debug, Clone)]
pub struct CachedDbTenantResolver<S: TenantStore, C: rume_data::cache::CacheBackend> {
    store: Arc<S>,
    cache: Arc<C>,
    extractor: TenantKeyExtractor,
    positive_ttl: std::time::Duration,
    negative_ttl: std::time::Duration,
}

impl<S: TenantStore, C: rume_data::cache::CacheBackend> CachedDbTenantResolver<S, C> {
    /// Creates a new [`CachedDbTenantResolver`] with default TTLs (300s positive, 30s negative).
    pub fn new(store: Arc<S>, cache: Arc<C>, extractor: TenantKeyExtractor) -> Self {
        Self {
            store,
            cache,
            extractor,
            positive_ttl: std::time::Duration::from_secs(300),
            negative_ttl: std::time::Duration::from_secs(30),
        }
    }

    /// Sets the positive cache TTL.
    pub fn with_positive_ttl(mut self, ttl: std::time::Duration) -> Self {
        self.positive_ttl = ttl;
        self
    }

    /// Sets the negative cache TTL.
    pub fn with_negative_ttl(mut self, ttl: std::time::Duration) -> Self {
        self.negative_ttl = ttl;
        self
    }

    fn cache_key(kind: &str, key: &str) -> String {
        format!("rume:tenant:{kind}:{key}")
    }

    async fn cache_get_entry(
        &self,
        cache_key: &str,
    ) -> Result<Option<CachedTenantEntry>, TenantError> {
        let Some(bytes) = self
            .cache
            .get(cache_key)
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))?
        else {
            return Ok(None);
        };
        let entry = serde_json::from_slice::<CachedTenantEntry>(&bytes)
            .map_err(|e| TenantError::Internal(format!("invalid cached tenant entry: {e}")))?;
        Ok(Some(entry))
    }

    async fn cache_set_entry(
        &self,
        cache_key: &str,
        entry: &CachedTenantEntry,
        ttl: std::time::Duration,
    ) -> Result<(), TenantError> {
        let bytes = serde_json::to_vec(entry).map_err(|e| {
            TenantError::Internal(format!("failed to serialize tenant cache entry: {e}"))
        })?;
        self.cache
            .set(cache_key, bytes, Some(ttl))
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))
    }

    async fn lookup_by_kind(&self, kind: &str, key: &str) -> Result<Option<Tenant>, TenantError> {
        let cache_key = Self::cache_key(kind, key);
        if let Some(entry) = self.cache_get_entry(&cache_key).await? {
            return match entry {
                CachedTenantEntry::Found(t) => Ok(Some(t)),
                CachedTenantEntry::NotFound => Ok(None),
            };
        }

        let db_result = match kind {
            "id" => self.store.find_by_id(key).await?,
            _ => self.store.find_by_slug(key).await?,
        };

        match db_result {
            Some(tenant) => {
                self.cache_set_entry(
                    &cache_key,
                    &CachedTenantEntry::Found(tenant.clone()),
                    self.positive_ttl,
                )
                .await?;
                Ok(Some(tenant))
            }
            None => {
                self.cache_set_entry(&cache_key, &CachedTenantEntry::NotFound, self.negative_ttl)
                    .await?;
                Ok(None)
            }
        }
    }

    /// Invalidates cached entries for `tenant_id` and `slug`.
    pub async fn invalidate(&self, tenant_id: &str, slug: &str) -> Result<(), TenantError> {
        self.cache
            .delete(&Self::cache_key("id", tenant_id))
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))?;
        self.cache
            .delete(&Self::cache_key("slug", slug))
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))?;
        Ok(())
    }
}

impl<S: TenantStore, C: rume_data::cache::CacheBackend + 'static> TenantResolver
    for CachedDbTenantResolver<S, C>
{
    fn resolve<'a>(
        &'a self,
        parts: &'a Parts,
    ) -> BoxFuture<'a, Result<Option<Tenant>, TenantError>> {
        Box::pin(async move {
            match &self.extractor {
                TenantKeyExtractor::Header(header_name) => {
                    let Some(key) = extract_header_key(parts, header_name)? else {
                        return Ok(None);
                    };
                    if let Some(tenant) = self.lookup_by_kind("id", key).await? {
                        return Ok(Some(tenant));
                    }
                    if let Some(tenant) = self.lookup_by_kind("slug", key).await? {
                        return Ok(Some(tenant));
                    }
                    Err(TenantError::TenantNotFound(key.to_string()))
                }
                TenantKeyExtractor::Subdomain(base_domain) => {
                    let base = normalize_base_domain(base_domain);
                    let Some(slug) = extract_subdomain_slug(parts, &base)? else {
                        return Ok(None);
                    };
                    match self.lookup_by_kind("slug", &slug).await? {
                        Some(tenant) => Ok(Some(tenant)),
                        None => Err(TenantError::TenantNotFound(slug)),
                    }
                }
                TenantKeyExtractor::PathPrefix(prefix) => {
                    let normalized = normalize_path_prefix(prefix);
                    let Some(slug) = extract_path_prefix_slug(parts, &normalized)? else {
                        return Ok(None);
                    };
                    match self.lookup_by_kind("slug", &slug).await? {
                        Some(tenant) => Ok(Some(tenant)),
                        None => Err(TenantError::TenantNotFound(slug)),
                    }
                }
            }
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

        let resolver = HeaderTenantResolver::new();
        assert!(resolver.try_add_tenant(tenant1.clone()).is_ok());
        // Colliding ID with existing slug fails closed with Conflict
        let conflict_err1 = resolver.try_add_tenant(tenant2.clone()).unwrap_err();
        assert!(matches!(conflict_err1, TenantError::Conflict(_)));

        // Reversed registration order: Tenant 2 registered before Tenant 1.
        // Adding Tenant 1 with slug "acme" must fail closed with Conflict against Tenant 2 whose ID is "acme".
        let resolver_rev = HeaderTenantResolver::new();
        assert!(resolver_rev.try_add_tenant(tenant2).is_ok());
        let conflict_err2 = resolver_rev.try_add_tenant(tenant1).unwrap_err();
        assert!(matches!(conflict_err2, TenantError::Conflict(_)));
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

    #[tokio::test]
    async fn subdomain_resolver_handles_mixed_case_base_http2_authority_and_fqdn_dot() {
        let tenant = Tenant::new("t-1", "acme");
        let resolver = SubdomainTenantResolver::new("MyPlatform.COM.").add_tenant(tenant.clone());

        // HTTP/2 :authority without Host header + trailing FQDN dot
        let (parts_h2, _) = Request::builder()
            .uri("https://ACME.myplatform.com.:443/api/items")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(resolver.resolve(&parts_h2).await.unwrap(), Some(tenant));
    }

    #[tokio::test]
    async fn path_prefix_tenant_resolver_extracts_slug_and_rejects_traversal_or_unknown() {
        let acme = Tenant::new("t-acme", "acme");
        let resolver = PathPrefixTenantResolver::new()
            .with_prefix("/t/")
            .add_tenant(acme.clone());

        let (parts_ok, _) = Request::builder()
            .uri("/t/acme/orders/42")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(resolver.resolve(&parts_ok).await.unwrap(), Some(acme));

        let (parts_traversal, _) = Request::builder()
            .uri("/t/../admin")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            resolver.resolve(&parts_traversal).await.unwrap_err(),
            TenantError::InvalidIdentifier(_)
        ));

        let (parts_encoded, _) = Request::builder()
            .uri("/t/acme%2fadmin/orders")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            resolver.resolve(&parts_encoded).await.unwrap_err(),
            TenantError::InvalidIdentifier(_)
        ));

        let (parts_unknown, _) = Request::builder()
            .uri("/t/unknown/orders")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            resolver.resolve(&parts_unknown).await.unwrap_err(),
            TenantError::TenantNotFound(s) if s == "unknown"
        ));
    }

    #[tokio::test]
    async fn path_prefix_tenant_resolver_returns_none_for_unprefixed_paths() {
        let acme = Tenant::new("t-acme", "acme");
        let resolver = PathPrefixTenantResolver::new().add_tenant(acme);

        let (parts_health, _) = Request::builder()
            .uri("/health/live")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(resolver.resolve(&parts_health).await.unwrap(), None);
    }

    #[tokio::test]
    async fn cached_db_tenant_resolver_caches_hits_negative_misses_and_invalidates() {
        use rume_data::cache::backend::{CacheBackend, InMemoryCacheBackend};
        use rume_data::persistence::backend::DatabaseBackend;
        use rume_data::persistence::sqlite::SqliteBackend;

        let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
        backend
            .execute(rume_data::sql::SQLITE_AUTH_SCHEMA)
            .await
            .unwrap();

        let store = Arc::new(SqlTenantStore::new(Arc::clone(&backend)));
        let cache = Arc::new(InMemoryCacheBackend::new());

        let acme = Tenant::new("t-acme", "acme");
        store.upsert(&acme).await.unwrap();

        let resolver = CachedDbTenantResolver::new(
            Arc::clone(&store),
            Arc::clone(&cache),
            TenantKeyExtractor::PathPrefix("/t/".into()),
        );

        // Positive lookup populates cache
        let (parts_ok, _) = Request::builder()
            .uri("/t/acme/dashboard")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(
            resolver.resolve(&parts_ok).await.unwrap(),
            Some(acme.clone())
        );
        assert!(cache.exists("rume:tenant:slug:acme").await.unwrap());

        // Deactivate in DB; cached positive entry still resolves until invalidated
        assert!(store.deactivate("t-acme").await.unwrap());
        assert_eq!(resolver.resolve(&parts_ok).await.unwrap(), Some(acme));

        // Explicit invalidation evicts cache; subsequent lookup sees DB deactivation and caches negative miss
        resolver.invalidate("t-acme", "acme").await.unwrap();
        assert!(!cache.exists("rume:tenant:slug:acme").await.unwrap());
        assert!(matches!(
            resolver.resolve(&parts_ok).await.unwrap_err(),
            TenantError::TenantNotFound(s) if s == "acme"
        ));
        assert!(cache.exists("rume:tenant:slug:acme").await.unwrap());

        // Re-upsert in DB; negative cache blocks until invalidated
        let acme_recreated = Tenant::new("t-acme", "acme");
        store.upsert(&acme_recreated).await.unwrap();
        assert!(matches!(
            resolver.resolve(&parts_ok).await.unwrap_err(),
            TenantError::TenantNotFound(_)
        ));
        resolver.invalidate("t-acme", "acme").await.unwrap();
        assert_eq!(
            resolver.resolve(&parts_ok).await.unwrap(),
            Some(acme_recreated)
        );
    }

    #[tokio::test]
    async fn resolvers_reject_duplicate_headers_and_invalid_tenant_header_values() {
        let acme = Tenant::new("t-acme", "acme");
        let header_resolver = HeaderTenantResolver::new().add_tenant(acme.clone());
        let sub_resolver = SubdomainTenantResolver::new("app.platform.io").add_tenant(acme);

        // Duplicate X-Tenant-ID rejected
        let (dup_tenant_parts, _) = Request::builder()
            .header("x-tenant-id", "acme")
            .header("x-tenant-id", "evil")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            header_resolver.resolve(&dup_tenant_parts).await,
            Err(TenantError::InvalidIdentifier(_))
        ));

        // Duplicate Host rejected
        let (dup_host_parts, _) = Request::builder()
            .header(HOST, "acme.app.platform.io")
            .header(HOST, "evil.app.platform.io")
            .body(())
            .unwrap()
            .into_parts();
        assert!(matches!(
            sub_resolver.resolve(&dup_host_parts).await,
            Err(TenantError::InvalidIdentifier(_))
        ));

        // Oversized (>128), empty, and unsafe charset X-Tenant-ID rejected
        let oversized = "a".repeat(129);
        for bad in ["", oversized.as_str(), "acme/admin", "acme;drop", "a b"] {
            let (parts, _) = Request::builder()
                .header("x-tenant-id", bad)
                .body(())
                .unwrap()
                .into_parts();
            assert!(
                matches!(
                    header_resolver.resolve(&parts).await,
                    Err(TenantError::InvalidIdentifier(_))
                ),
                "expected InvalidIdentifier for header value {bad:?}"
            );
        }
    }
}
