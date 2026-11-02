//! Tenant-isolated cache scoping and bulk invalidation.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;

use crate::cache::backend::CacheBackend;
use crate::cache::error::CacheError;
use crate::cache::typed::Cache;

/// A cache wrapper that automatically isolates data per tenant using key prefixes and invalidation tags.
#[derive(Clone)]
pub struct TenantScopedCache<B: CacheBackend> {
    cache: Cache<B>,
}

impl<B: CacheBackend> TenantScopedCache<B> {
    /// Creates a new tenant-scoped cache wrapper.
    pub fn new(cache: Cache<B>) -> Self {
        Self { cache }
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
        let full_key = Self::scoped_key(tenant_id, key);
        let tag = Self::tenant_tag(tenant_id);
        self.cache
            .set_json_with_tags(&full_key, value, &[&tag], ttl)
            .await
    }

    /// Deletes a specific key scoped to `tenant_id`.
    pub async fn delete(&self, tenant_id: &str, key: &str) -> Result<bool, CacheError> {
        let full_key = Self::scoped_key(tenant_id, key);
        self.cache.delete(&full_key).await
    }

    /// Evicts all cached entries belonging to `tenant_id`.
    pub async fn invalidate_tenant(&self, tenant_id: &str) -> Result<usize, CacheError> {
        let tag = Self::tenant_tag(tenant_id);
        self.cache.invalidate_tag(&tag).await
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
