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

    fn scoped_key(tenant_id: &str, key: &str) -> String {
        format!("tenant:{tenant_id}:{key}")
    }

    fn tenant_tag(tenant_id: &str) -> String {
        format!("tenant:{tenant_id}")
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
