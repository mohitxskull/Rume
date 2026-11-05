//! Tenant-isolated cache scoping and bulk invalidation.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::cache::backend::CacheBackend;
use crate::cache::error::CacheError;
use crate::cache::typed::Cache;
use crate::tenant::quota::TenantQuotaStore;

/// Returns the disambiguated length-prefixed cache key scoped to `tenant_id`.
pub fn scoped_cache_key(tenant_id: &str, key: &str) -> String {
    format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)
}

/// Returns the disambiguated length-prefixed cache tag scoped to `tenant_id`.
pub fn tenant_cache_tag(tenant_id: &str) -> String {
    format!("tenant:{}:{tenant_id}", tenant_id.len())
}

/// A cache wrapper that automatically isolates data per tenant using key prefixes, namespaced tags, and optional quotas.
#[derive(Clone)]
pub struct TenantScopedCache<B: CacheBackend> {
    cache: Cache<B>,
    max_keys_per_tenant: usize,
    quota_store: Option<Arc<dyn TenantQuotaStore>>,
}

impl<B: CacheBackend> TenantScopedCache<B> {
    /// Default maximum cached keys per tenant.
    pub const DEFAULT_MAX_KEYS_PER_TENANT: usize = 5_000;

    /// Creates a new tenant-scoped cache wrapper without quota limits.
    pub fn new(cache: Cache<B>) -> Self {
        Self {
            cache,
            max_keys_per_tenant: Self::DEFAULT_MAX_KEYS_PER_TENANT,
            quota_store: None,
        }
    }

    /// Sets the maximum number of keys permitted per tenant.
    pub fn with_max_keys_per_tenant(mut self, max_keys: usize) -> Self {
        self.max_keys_per_tenant = max_keys.max(1);
        self
    }

    /// Configures a custom [`TenantQuotaStore`] (e.g. [`RedisTenantQuotaStore`] for distributed quotas).
    pub fn with_quota_store(mut self, quota_store: Arc<dyn TenantQuotaStore>) -> Self {
        self.quota_store = Some(quota_store);
        self
    }

    /// Resolves the length-prefixed tenant cache key.
    pub fn scoped_key(tenant_id: &str, key: &str) -> String {
        scoped_cache_key(tenant_id, key)
    }

    /// Resolves the length-prefixed tenant invalidation tag.
    pub fn tenant_tag(tenant_id: &str) -> String {
        tenant_cache_tag(tenant_id)
    }

    fn scoped_tag(tenant_id: &str, tag: &str) -> String {
        format!("tenant:{}:{}:tag:{tag}", tenant_id.len(), tenant_id)
    }

    /// Retrieves a cached value scoped to `tenant_id`.
    pub async fn get<T: DeserializeOwned>(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> Result<Option<T>, CacheError> {
        let full_key = Self::scoped_key(tenant_id, key);
        self.cache.get_json(&full_key).await
    }

    /// Stores a cached value scoped to `tenant_id` and tagged for tenant-wide invalidation.
    pub async fn set<T: Serialize>(
        &self,
        tenant_id: &str,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        self.set_with_tags(tenant_id, key, value, &[], ttl).await
    }

    /// Stores a cached value scoped to `tenant_id` with custom invalidation tags and tenant-level quota enforcement.
    pub async fn set_with_tags<T: Serialize>(
        &self,
        tenant_id: &str,
        key: &str,
        value: &T,
        tags: &[&str],
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        let full_key = Self::scoped_key(tenant_id, key);

        if let Some(ref quota) = self.quota_store {
            let acquired = quota
                .record_key(tenant_id, key, ttl, self.max_keys_per_tenant)
                .await?;
            if !acquired {
                return Err(CacheError::QuotaExceeded(format!(
                    "tenant '{tenant_id}' exceeded quota of {} keys",
                    self.max_keys_per_tenant
                )));
            }
        }

        let tenant_tag = Self::tenant_tag(tenant_id);
        let mut all_tags = vec![tenant_tag];
        for t in tags {
            all_tags.push(Self::scoped_tag(tenant_id, t));
        }
        let tag_refs: Vec<&str> = all_tags.iter().map(|s| s.as_str()).collect();

        let res = self
            .cache
            .set_json_with_tags(&full_key, value, &tag_refs, ttl)
            .await;

        if res.is_err()
            && let Some(ref quota) = self.quota_store
        {
            let _ = quota.remove_key(tenant_id, key).await;
        }

        res
    }

    /// Deletes a specific key scoped to `tenant_id`.
    pub async fn delete(&self, tenant_id: &str, key: &str) -> Result<bool, CacheError> {
        let full_key = Self::scoped_key(tenant_id, key);
        let deleted = self.cache.delete(&full_key).await?;
        if deleted && let Some(ref quota) = self.quota_store {
            let _ = quota.remove_key(tenant_id, key).await;
        }
        Ok(deleted)
    }

    /// Evicts all cached entries belonging to `tenant_id` matching `tag`.
    pub async fn invalidate_tag(&self, tenant_id: &str, tag: &str) -> Result<usize, CacheError> {
        let scoped = Self::scoped_tag(tenant_id, tag);
        let deleted_keys = self.cache.invalidate_tag_keys(&scoped).await?;
        let count = deleted_keys.len();
        if let Some(ref quota) = self.quota_store {
            let prefix = format!("tenant:{}:{}:", tenant_id.len(), tenant_id);
            for k in &deleted_keys {
                if let Some(raw_key) = k.strip_prefix(&prefix) {
                    let _ = quota.remove_key(tenant_id, raw_key).await;
                }
            }
        }
        Ok(count)
    }

    /// Evicts all cached entries belonging to `tenant_id`.
    pub async fn invalidate_tenant(&self, tenant_id: &str) -> Result<usize, CacheError> {
        let tag = Self::tenant_tag(tenant_id);
        let deleted_keys = self.cache.invalidate_tag_keys(&tag).await?;
        let count = deleted_keys.len();
        if let Some(ref quota) = self.quota_store {
            let _ = quota.reset(tenant_id).await;
        }
        Ok(count)
    }

    /// Returns the active number of cached keys for `tenant_id`.
    pub async fn active_key_count(&self, tenant_id: &str) -> Result<usize, CacheError> {
        if let Some(ref quota) = self.quota_store {
            quota.count(tenant_id).await
        } else {
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_key_disambiguates_colon_delimited_tenant_id_and_key() {
        let key1 = scoped_cache_key("a:b", "c");
        let key2 = scoped_cache_key("a", "b:c");
        assert_eq!(key1, "tenant:3:a:b:c");
        assert_eq!(key2, "tenant:1:a:b:c");
        assert_ne!(key1, key2);
    }

    #[test]
    fn tenant_tag_disambiguates_colon_delimited_tenant_ids() {
        let tag1 = tenant_cache_tag("a:b");
        let tag2 = tenant_cache_tag("a");
        assert_eq!(tag1, "tenant:3:a:b");
        assert_eq!(tag2, "tenant:1:a");
        assert_ne!(tag1, tag2);
    }
}
