//! Tenant-isolated cache scoping and bulk invalidation.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

use crate::cache::backend::CacheBackend;
use crate::cache::error::CacheError;
use crate::cache::typed::Cache;
use crate::tenant::quota::{MemoryTenantQuotaStore, TenantQuotaStore};

use std::sync::Arc;

/// A cache wrapper that automatically isolates data per tenant using key prefixes, namespaced tags, and quotas.
#[derive(Clone)]
pub struct TenantScopedCache<B: CacheBackend> {
    cache: Cache<B>,
    max_keys_per_tenant: usize,
    quota_store: Arc<dyn TenantQuotaStore>,
}

impl<B: CacheBackend> TenantScopedCache<B> {
    /// Default maximum cached keys per tenant.
    pub const DEFAULT_MAX_KEYS_PER_TENANT: usize = 5_000;

    /// Creates a new tenant-scoped cache wrapper with default quotas and in-memory quota tracking.
    pub fn new(cache: Cache<B>) -> Self {
        Self {
            cache,
            max_keys_per_tenant: Self::DEFAULT_MAX_KEYS_PER_TENANT,
            quota_store: Arc::new(MemoryTenantQuotaStore::new()),
        }
    }

    /// Sets the maximum number of keys permitted per tenant.
    pub fn with_max_keys_per_tenant(mut self, max_keys: usize) -> Self {
        self.max_keys_per_tenant = max_keys.max(1);
        self
    }

    /// Configures a custom [`TenantQuotaStore`] (e.g. [`RedisTenantQuotaStore`] for distributed quotas).
    pub fn with_quota_store(mut self, quota_store: Arc<dyn TenantQuotaStore>) -> Self {
        self.quota_store = quota_store;
        self
    }

    // Why: Length-prefixing the tenant segment ("tenant:{len}:{tenant_id}:{key}") guarantees
    // unambiguous key segment boundaries without restricting allowed characters in tenant IDs
    // or keys (e.g. UUIDs, slashes, hyphens, or legacy colon identifiers).
    fn scoped_key(tenant_id: &str, key: &str) -> String {
        format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)
    }

    // Why: Symmetrical length-prefixing on tenant invalidation tags ("tenant:{len}:{tenant_id}")
    // prevents tag collision where invalidating tenant "a" could inadvertently evict tenant "a:b".
    fn tenant_tag(tenant_id: &str) -> String {
        format!("tenant:{}:{tenant_id}", tenant_id.len())
    }

    // Why: Symmetrical length-prefixing on user-defined tags scoped to tenant
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
        let exists = self.cache.exists(&full_key).await?;

        if !exists {
            let acquired = self
                .quota_store
                .acquire(tenant_id, self.max_keys_per_tenant)
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

        self.cache
            .set_json_with_tags(&full_key, value, &tag_refs, ttl)
            .await
    }

    /// Deletes a specific key scoped to `tenant_id`.
    pub async fn delete(&self, tenant_id: &str, key: &str) -> Result<bool, CacheError> {
        let full_key = Self::scoped_key(tenant_id, key);
        let deleted = self.cache.delete(&full_key).await?;
        if deleted {
            let _ = self.quota_store.release(tenant_id).await;
        }
        Ok(deleted)
    }

    /// Evicts all cached entries belonging to `tenant_id` matching `tag`.
    pub async fn invalidate_tag(&self, tenant_id: &str, tag: &str) -> Result<usize, CacheError> {
        let scoped = Self::scoped_tag(tenant_id, tag);
        let count = self.cache.invalidate_tag(&scoped).await?;
        for _ in 0..count {
            let _ = self.quota_store.release(tenant_id).await;
        }
        Ok(count)
    }

    /// Evicts all cached entries belonging to `tenant_id`.
    pub async fn invalidate_tenant(&self, tenant_id: &str) -> Result<usize, CacheError> {
        let tag = Self::tenant_tag(tenant_id);
        let count = self.cache.invalidate_tag(&tag).await?;
        let _ = self.quota_store.reset(tenant_id).await;
        Ok(count)
    }

    /// Returns the active number of cached keys for `tenant_id`.
    pub async fn active_key_count(&self, tenant_id: &str) -> Result<usize, CacheError> {
        self.quota_store.count(tenant_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{Cache, CacheConfig, MemoryCacheBackend};
    use serde::{Deserialize, Serialize};

    #[test]
    fn scoped_key_disambiguates_colon_delimited_tenant_id_and_key() {
        let key1 = TenantScopedCache::<MemoryCacheBackend>::scoped_key("a:b", "c");
        let key2 = TenantScopedCache::<MemoryCacheBackend>::scoped_key("a", "b:c");
        assert_eq!(key1, "tenant:3:a:b:c");
        assert_eq!(key2, "tenant:1:a:b:c");
        assert_ne!(key1, key2);
    }

    #[test]
    fn tenant_tag_disambiguates_colon_delimited_tenant_ids() {
        let tag1 = TenantScopedCache::<MemoryCacheBackend>::tenant_tag("a:b");
        let tag2 = TenantScopedCache::<MemoryCacheBackend>::tenant_tag("a");
        assert_eq!(tag1, "tenant:3:a:b");
        assert_eq!(tag2, "tenant:1:a");
        assert_ne!(tag1, tag2);
    }

    #[tokio::test]
    async fn tenant_scoped_cache_preserves_isolated_read_and_write_with_colons() {
        let base_cache = Cache::new(MemoryCacheBackend::new(), CacheConfig::default());
        let scoped_cache = TenantScopedCache::new(base_cache);

        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct Data {
            value: String,
        }

        scoped_cache
            .set(
                "t1:user",
                "42",
                &Data {
                    value: "tenant_t1_user_data".to_string(),
                },
                None,
            )
            .await
            .unwrap();

        scoped_cache
            .set(
                "t1",
                "user:42",
                &Data {
                    value: "tenant_t1_data".to_string(),
                },
                None,
            )
            .await
            .unwrap();

        let val_t1: Option<Data> = scoped_cache.get("t1", "user:42").await.unwrap();
        let val_t1_user: Option<Data> = scoped_cache.get("t1:user", "42").await.unwrap();

        assert_eq!(
            val_t1,
            Some(Data {
                value: "tenant_t1_data".to_string()
            })
        );
        assert_eq!(
            val_t1_user,
            Some(Data {
                value: "tenant_t1_user_data".to_string()
            })
        );
    }

    #[tokio::test]
    async fn tenant_scoped_cache_invalidation_does_not_leak_across_colon_boundaries() {
        let base_cache = Cache::new(MemoryCacheBackend::new(), CacheConfig::default());
        let scoped_cache = TenantScopedCache::new(base_cache);

        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct Data {
            value: String,
        }

        scoped_cache
            .set(
                "t1:user",
                "42",
                &Data {
                    value: "preserved".to_string(),
                },
                None,
            )
            .await
            .unwrap();

        scoped_cache
            .set(
                "t1",
                "user:42",
                &Data {
                    value: "to_evict".to_string(),
                },
                None,
            )
            .await
            .unwrap();

        let invalidated = scoped_cache.invalidate_tenant("t1").await.unwrap();
        assert_eq!(invalidated, 1);

        let val_t1: Option<Data> = scoped_cache.get("t1", "user:42").await.unwrap();
        let val_t1_user: Option<Data> = scoped_cache.get("t1:user", "42").await.unwrap();

        assert_eq!(val_t1, None);
        assert_eq!(
            val_t1_user,
            Some(Data {
                value: "preserved".to_string()
            })
        );
    }
}
