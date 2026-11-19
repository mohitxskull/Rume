//! Tenant-isolated cache scoping and bulk invalidation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::tenant::quota::{KeyReservation, TenantQuotaStore};
use rume_data::cache::backend::CacheBackend;
use rume_data::cache::error::CacheError;
use rume_data::cache::typed::Cache;

type KeyLockMap = Arc<std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>;

/// Per-key async write/delete serialization guard with automatic map eviction when idle.
///
/// Why: `TenantScopedCache::set_with_tags` coordinates two independent stores (`quota_store`
/// and `cache`) across three async steps (`reserve_key` -> `set_json_with_tags` -> `record_key`
/// or `remove_key`). Serializing same-key mutations ensures concurrent writers A and B on the
/// same `(tenant_id, key)` cannot interleave reservation and rollback to delete each other's
/// active quota accounting.
struct KeyLockGuard {
    map: KeyLockMap,
    key: String,
    mutex: Arc<tokio::sync::Mutex<()>>,
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl KeyLockGuard {
    async fn acquire(map: &KeyLockMap, key: &str) -> Self {
        let mutex = {
            let mut locks = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::clone(
                locks
                    .entry(key.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let guard = Arc::clone(&mutex).lock_owned().await;
        Self {
            map: Arc::clone(map),
            key: key.to_string(),
            mutex,
            _guard: Some(guard),
        }
    }
}

impl Drop for KeyLockGuard {
    fn drop(&mut self) {
        let mut locks = self
            .map
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // While `_guard` is still held, references to `self.mutex` are:
        // 1 in `locks` map + 1 in `self.mutex` + 1 in `self._guard` == 3 when no other caller
        // is waiting on this key. Evicting under `locks` while `strong_count == 3` is race-free.
        if let Some(current) = locks.get(&self.key)
            && Arc::ptr_eq(current, &self.mutex)
            && Arc::strong_count(&self.mutex) == 3
        {
            locks.remove(&self.key);
        }
        drop(self._guard.take());
    }
}

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
    key_locks: KeyLockMap,
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
            key_locks: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
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
        // Serialize same-key write/delete lifecycles so concurrent writers on `(tenant_id, key)`
        // never interleave `reserve_key`, backend publication, finalization, or rollback.
        let _key_lock = KeyLockGuard::acquire(&self.key_locks, &full_key).await;

        // Pre-validate JSON serialization before touching `quota_store` so a serialization failure
        // never creates or rolls back a quota reservation.
        let _ = serde_json::to_vec(value).map_err(|e| CacheError::Serialization(e.to_string()))?;

        // Why: `Cache::set_json_with_tags` substitutes `Cache::default_ttl()` when `ttl` is
        // `None`. Passing raw `ttl` to `quota.reserve_key` / `record_key` would record an
        // unbounded (`+inf`) quota entry while the cache entry expires after `default_ttl()`,
        // permanently leaking tenant quota slots. Resolving once up front guarantees quota
        // and cache share one TTL.
        let effective_ttl = self.cache.resolve_ttl(ttl);

        // Why: `reserve_key` inserts a new slot only when the key is not already tracked
        // and leaves an existing key's expiration untouched. That way (1) a failed overwrite
        // never shortens an existing reservation's TTL, and (2) rollback only removes a slot
        // owned by this call (`NewlyReserved`), never evicting a pre-existing reservation even
        // when `exists()` also fails during a backend outage.
        let reservation = if let Some(ref quota) = self.quota_store {
            let outcome = quota
                .reserve_key(tenant_id, key, effective_ttl, self.max_keys_per_tenant)
                .await?;
            if outcome == KeyReservation::QuotaExceeded {
                return Err(CacheError::QuotaExceeded(format!(
                    "tenant '{tenant_id}' exceeded quota of {} keys",
                    self.max_keys_per_tenant
                )));
            }
            Some(outcome)
        } else {
            None
        };

        let tenant_tag = Self::tenant_tag(tenant_id);
        let mut all_tags = vec![tenant_tag];
        for t in tags {
            all_tags.push(Self::scoped_tag(tenant_id, t));
        }
        let tag_refs: Vec<&str> = all_tags.iter().map(|s| s.as_str()).collect();

        let res = self
            .cache
            .set_json_with_tags(&full_key, value, &tag_refs, effective_ttl)
            .await;

        let Some(ref quota) = self.quota_store else {
            return res;
        };

        match (&res, reservation) {
            (Ok(()), Some(KeyReservation::NewlyReserved | KeyReservation::AlreadyTracked)) => {
                // Why: finalize quota accounting after publication for BOTH `NewlyReserved` and
                // `AlreadyTracked`. The initial reservation may have expired while a slow backend
                // write was in flight and another key may have taken the slot while the cache
                // write started a fresh entry TTL; `record_key` refreshes the quota TTL to match
                // post-publication time or rejects if the slot was lost. If rejected or errored,
                // evict the newly written entry so no live cache key ever escapes quota tracking.
                match quota
                    .record_key(tenant_id, key, effective_ttl, self.max_keys_per_tenant)
                    .await
                {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        let _ = self.cache.delete(&full_key).await;
                        Err(CacheError::QuotaExceeded(format!(
                            "tenant '{tenant_id}' exceeded quota of {} keys while finalizing write",
                            self.max_keys_per_tenant
                        )))
                    }
                    Err(err) => {
                        let _ = self.cache.delete(&full_key).await;
                        Err(err)
                    }
                }
            }
            (Err(err), Some(KeyReservation::NewlyReserved)) => {
                // Why: roll back the slot this call created only when the write is KNOWN not to
                // have applied. `Serialization`/`Closed` fail before reaching the backend. For
                // `Backend`/`Connection` errors the write is ambiguous (the Lua script may have
                // run before the reply was lost), so remove the slot only if `exists` positively
                // reports the entry absent. If `exists` also fails, keep the reservation: a live
                // entry must never lose its quota slot, and the slot still expires with its TTL.
                let definitely_not_written =
                    matches!(err, CacheError::Serialization(_) | CacheError::Closed);
                let absent = definitely_not_written
                    || matches!(self.cache.exists(&full_key).await, Ok(false));
                if absent {
                    let _ = quota.remove_key(tenant_id, key).await;
                }
                res
            }
            _ => res,
        }
    }

    /// Deletes a specific key scoped to `tenant_id`.
    pub async fn delete(&self, tenant_id: &str, key: &str) -> Result<bool, CacheError> {
        let full_key = Self::scoped_key(tenant_id, key);
        let _key_lock = KeyLockGuard::acquire(&self.key_locks, &full_key).await;
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
    use crate::tenant::quota::InMemoryTenantQuotaStore;

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

    #[derive(Debug, Default)]
    struct SpyQuotaStore {
        inner: InMemoryTenantQuotaStore,
        recorded_ttls: std::sync::Mutex<Vec<(String, Option<Duration>)>>,
    }

    impl TenantQuotaStore for SpyQuotaStore {
        fn acquire<'a>(
            &'a self,
            tenant_id: &'a str,
            max_keys: usize,
            window: Duration,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<bool, CacheError>> {
            self.inner.acquire(tenant_id, max_keys, window)
        }

        fn release<'a>(
            &'a self,
            tenant_id: &'a str,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<(), CacheError>> {
            self.inner.release(tenant_id)
        }

        fn record_key<'a>(
            &'a self,
            tenant_id: &'a str,
            key: &'a str,
            ttl: Option<Duration>,
            max_keys: usize,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<bool, CacheError>> {
            self.recorded_ttls
                .lock()
                .expect("spy mutex poisoned")
                .push((key.to_string(), ttl));
            self.inner.record_key(tenant_id, key, ttl, max_keys)
        }

        fn reserve_key<'a>(
            &'a self,
            tenant_id: &'a str,
            key: &'a str,
            ttl: Option<Duration>,
            max_keys: usize,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<KeyReservation, CacheError>> {
            self.inner.reserve_key(tenant_id, key, ttl, max_keys)
        }

        fn remove_key<'a>(
            &'a self,
            tenant_id: &'a str,
            key: &'a str,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<(), CacheError>> {
            self.inner.remove_key(tenant_id, key)
        }

        fn count<'a>(
            &'a self,
            tenant_id: &'a str,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<usize, CacheError>> {
            self.inner.count(tenant_id)
        }

        fn reset<'a>(
            &'a self,
            tenant_id: &'a str,
        ) -> crate::tenant::quota::BoxFuture<'a, Result<(), CacheError>> {
            self.inner.reset(tenant_id)
        }
    }

    #[derive(Debug)]
    struct FailingBackend {
        inner: rume_data::cache::backend::InMemoryCacheBackend,
        fail_writes: Arc<std::sync::atomic::AtomicBool>,
        fail_exists: Arc<std::sync::atomic::AtomicBool>,
        /// Milliseconds each successful tagged write waits before applying (paused-time tests).
        write_delay_ms: Arc<std::sync::atomic::AtomicU64>,
    }

    impl FailingBackend {
        fn new(
            fail_writes: Arc<std::sync::atomic::AtomicBool>,
            fail_exists: Arc<std::sync::atomic::AtomicBool>,
        ) -> Self {
            Self {
                inner: rume_data::cache::backend::InMemoryCacheBackend::new(),
                fail_writes,
                fail_exists,
                write_delay_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            }
        }
    }

    impl CacheBackend for FailingBackend {
        fn get(
            &self,
            key: &str,
        ) -> rume_data::cache::backend::BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
            self.inner.get(key)
        }

        fn set(
            &self,
            key: &str,
            value: Vec<u8>,
            ttl: Option<Duration>,
        ) -> rume_data::cache::backend::BoxFuture<'_, Result<(), CacheError>> {
            self.inner.set(key, value, ttl)
        }

        fn set_with_tags(
            &self,
            key: &str,
            value: Vec<u8>,
            tags: &[&str],
            ttl: Option<Duration>,
        ) -> rume_data::cache::backend::BoxFuture<'_, Result<(), CacheError>> {
            if self.fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
                return Box::pin(async { Err(CacheError::Backend("redis unreachable".into())) });
            }
            let delay = self
                .write_delay_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            let write = self.inner.set_with_tags(key, value, tags, ttl);
            Box::pin(async move {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                write.await
            })
        }

        fn delete(
            &self,
            key: &str,
        ) -> rume_data::cache::backend::BoxFuture<'_, Result<bool, CacheError>> {
            self.inner.delete(key)
        }

        fn exists(
            &self,
            key: &str,
        ) -> rume_data::cache::backend::BoxFuture<'_, Result<bool, CacheError>> {
            if self.fail_exists.load(std::sync::atomic::Ordering::SeqCst) {
                return Box::pin(async { Err(CacheError::Backend("redis unreachable".into())) });
            }
            self.inner.exists(key)
        }

        fn invalidate_tag_keys(
            &self,
            tag: &str,
        ) -> rume_data::cache::backend::BoxFuture<'_, Result<Vec<String>, CacheError>> {
            self.inner.invalidate_tag_keys(tag)
        }

        fn clear(&self) -> rume_data::cache::backend::BoxFuture<'_, Result<(), CacheError>> {
            self.inner.clear()
        }

        fn ping(&self) -> rume_data::cache::backend::BoxFuture<'_, Result<(), CacheError>> {
            self.inner.ping()
        }
    }

    #[tokio::test]
    async fn tenant_cache_none_ttl_propagates_cache_default_ttl_to_quota_store() {
        let config = rume_data::cache::config::CacheConfig::default().with_default_ttl(42);
        let cache = Cache::new(
            rume_data::cache::backend::InMemoryCacheBackend::new(),
            config,
        );
        assert_eq!(cache.default_ttl(), Duration::from_secs(42));
        assert_eq!(cache.resolve_ttl(None), Some(Duration::from_secs(42)));
        let spy = Arc::new(SpyQuotaStore::default());
        let tenant_cache = TenantScopedCache::new(cache).with_quota_store(Arc::clone(&spy) as _);

        tenant_cache
            .set("t1", "k-default", &"v1", None)
            .await
            .expect("set with default ttl");
        tenant_cache
            .set("t1", "k-explicit", &"v2", Some(Duration::from_secs(7)))
            .await
            .expect("set with explicit ttl");

        let calls = spy
            .recorded_ttls
            .lock()
            .expect("spy mutex poisoned")
            .clone();
        assert_eq!(
            calls,
            vec![
                ("k-default".to_string(), Some(Duration::from_secs(42))),
                ("k-explicit".to_string(), Some(Duration::from_secs(7))),
            ]
        );
    }

    #[tokio::test]
    async fn tenant_cache_rolls_back_quota_on_closed_and_backend_errors() {
        use crate::tenant::quota::InMemoryTenantQuotaStore;

        // 1. Closed cache fails set_json_with_tags AND exists() with CacheError::Closed.
        let closed_cache = Cache::new(
            rume_data::cache::backend::InMemoryCacheBackend::new(),
            rume_data::cache::config::CacheConfig::default(),
        );
        let closed_quota = Arc::new(InMemoryTenantQuotaStore::new());
        let closed_tenant_cache = TenantScopedCache::new(closed_cache.clone())
            .with_quota_store(Arc::clone(&closed_quota) as _);
        closed_cache.close();

        let err = closed_tenant_cache
            .set("t1", "closed-key", &"val", None)
            .await
            .expect_err("closed cache must fail set");
        assert!(matches!(err, CacheError::Closed));
        assert_eq!(
            closed_tenant_cache
                .active_key_count("t1")
                .await
                .expect("count"),
            0,
            "quota slot must be rolled back when cache is closed"
        );

        // 2. Failing backend fails both set_with_tags and exists with CacheError::Backend.
        let fail_flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let failing_cache = Cache::new(
            FailingBackend::new(Arc::clone(&fail_flag), Arc::clone(&fail_flag)),
            rume_data::cache::config::CacheConfig::default(),
        );
        let backend_quota = Arc::new(InMemoryTenantQuotaStore::new());
        let backend_tenant_cache =
            TenantScopedCache::new(failing_cache).with_quota_store(Arc::clone(&backend_quota) as _);

        let err = backend_tenant_cache
            .set("t1", "backend-key", &"val", None)
            .await
            .expect_err("failing backend must return Backend error");
        assert!(matches!(err, CacheError::Backend(_)));
        assert_eq!(
            backend_tenant_cache
                .active_key_count("t1")
                .await
                .expect("count"),
            1,
            "ambiguous backend error with a failing exists() must keep the reservation"
        );

        // 2b. A backend write error is rolled back once `exists()` positively reports absence.
        let write_only_fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let exists_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let absent_cache = Cache::new(
            FailingBackend::new(Arc::clone(&write_only_fail), Arc::clone(&exists_ok)),
            rume_data::cache::config::CacheConfig::default(),
        );
        let absent_tenant_cache = TenantScopedCache::new(absent_cache)
            .with_quota_store(Arc::new(InMemoryTenantQuotaStore::new()) as _);
        absent_tenant_cache
            .set("t1", "absent-key", &"val", None)
            .await
            .expect_err("failing write must return Backend error");
        assert_eq!(
            absent_tenant_cache
                .active_key_count("t1")
                .await
                .expect("count"),
            0,
            "slot must be rolled back when exists() confirms the entry was not written"
        );

        // 3. Updating an already-existing key that fails write preserves its existing quota slot.
        let fail_writes = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let partial_cache = Cache::new(
            FailingBackend::new(Arc::clone(&fail_writes), Arc::clone(&fail_exists)),
            rume_data::cache::config::CacheConfig::default(),
        );
        let existing_quota = Arc::new(InMemoryTenantQuotaStore::new());
        let existing_tenant_cache = TenantScopedCache::new(partial_cache)
            .with_quota_store(Arc::clone(&existing_quota) as _);

        existing_tenant_cache
            .set("t1", "existing-key", &"v1", None)
            .await
            .expect("initial set succeeds");
        fail_writes.store(true, std::sync::atomic::Ordering::SeqCst);
        let update_err = existing_tenant_cache
            .set("t1", "existing-key", &"v2", None)
            .await
            .expect_err("failed overwrite must return Backend error");
        assert!(matches!(update_err, CacheError::Backend(_)));
        assert_eq!(
            existing_tenant_cache
                .active_key_count("t1")
                .await
                .expect("count"),
            1,
            "existing key quota slot must not be removed when overwrite fails"
        );

        // 4. Existing reservation must be preserved even when BOTH write and exists() fail (#29).
        fail_exists.store(true, std::sync::atomic::Ordering::SeqCst);
        let both_fail_err = existing_tenant_cache
            .set("t1", "existing-key", &"v3", None)
            .await
            .expect_err("failed overwrite with failing exists must return Backend error");
        assert!(matches!(both_fail_err, CacheError::Backend(_)));
        assert_eq!(
            existing_tenant_cache
                .active_key_count("t1")
                .await
                .expect("count"),
            1,
            "existing key quota slot must survive when both write and exists() fail"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn tenant_cache_failed_overwrite_preserves_previous_quota_expiration() {
        let fail_writes = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let config = rume_data::cache::config::CacheConfig::default().with_default_ttl(42);
        let cache = Cache::new(
            FailingBackend::new(Arc::clone(&fail_writes), Arc::clone(&fail_exists)),
            config,
        );
        let quota = Arc::new(InMemoryTenantQuotaStore::new());
        let tenant_cache = TenantScopedCache::new(cache)
            .with_max_keys_per_tenant(1)
            .with_quota_store(Arc::clone(&quota) as _);

        // Write with a 1-hour TTL.
        tenant_cache
            .set("t1", "k1", &"v1", Some(Duration::from_secs(3600)))
            .await
            .expect("initial 1h write succeeds");

        // Fail an overwrite with `ttl = None` (which resolves to 42s default).
        fail_writes.store(true, std::sync::atomic::Ordering::SeqCst);
        let err = tenant_cache
            .set("t1", "k1", &"v2", None)
            .await
            .expect_err("failed overwrite must return Backend error");
        assert!(matches!(err, CacheError::Backend(_)));

        // Advance past the 42s default TTL; the original 1h reservation must still be active.
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(
            tenant_cache.active_key_count("t1").await.expect("count"),
            1,
            "failed overwrite with shorter TTL must not shorten the existing reservation"
        );

        // Once writes recover, a successful overwrite with `ttl = None` refreshes to 42s.
        fail_writes.store(false, std::sync::atomic::Ordering::SeqCst);
        tenant_cache
            .set("t1", "k1", &"v3", None)
            .await
            .expect("successful overwrite refreshes TTL");
        tokio::time::advance(Duration::from_secs(43)).await;
        assert_eq!(
            tenant_cache.active_key_count("t1").await.expect("count"),
            0,
            "successful overwrite must refresh the reservation to the new 42s TTL"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn overwrite_finalization_rejection_evicts_untracked_entry() {
        let fail_writes = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let backend = FailingBackend::new(Arc::clone(&fail_writes), Arc::clone(&fail_exists));
        let write_delay_ms = Arc::clone(&backend.write_delay_ms);
        let cache = Cache::new(backend, rume_data::cache::config::CacheConfig::default());
        let quota = Arc::new(InMemoryTenantQuotaStore::new());
        let tenant_cache = Arc::new(
            TenantScopedCache::new(cache)
                .with_max_keys_per_tenant(1)
                .with_quota_store(Arc::clone(&quota) as _),
        );

        // K is tracked with a 10s reservation.
        tenant_cache
            .set("t1", "k", &"v1", Some(Duration::from_secs(10)))
            .await
            .expect("initial write");

        // K's overwrite stays in flight for 20s while its reservation expires at t=10s and a
        // different key L takes the only slot at t=15s.
        write_delay_ms.store(20_000, std::sync::atomic::Ordering::SeqCst);
        let overwrite = {
            let c = Arc::clone(&tenant_cache);
            async move { c.set("t1", "k", &"v2", Some(Duration::from_secs(60))).await }
        };
        let steal = {
            let c = Arc::clone(&tenant_cache);
            let delay = Arc::clone(&write_delay_ms);
            async move {
                tokio::time::sleep(Duration::from_secs(15)).await;
                delay.store(0, std::sync::atomic::Ordering::SeqCst);
                c.set("t1", "l", &"vl", Some(Duration::from_secs(60))).await
            }
        };
        let (overwrite_res, steal_res) = tokio::join!(overwrite, steal);

        steal_res.expect("L takes the slot freed by K's expired reservation");
        let err =
            overwrite_res.expect_err("K's finalization must be rejected, not reported as success");
        assert!(matches!(err, CacheError::QuotaExceeded(_)), "got {err:?}");
        assert_eq!(
            tenant_cache.get::<String>("t1", "k").await.expect("get"),
            None,
            "untracked K must be evicted so no live entry escapes quota accounting"
        );
        assert_eq!(
            tenant_cache.active_key_count("t1").await.expect("count"),
            1,
            "only L remains tracked"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn newly_reserved_write_finalizes_after_publication_and_evicts_if_slot_expired() {
        let fail_writes = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let backend = FailingBackend::new(Arc::clone(&fail_writes), Arc::clone(&fail_exists));
        let write_delay_ms = Arc::clone(&backend.write_delay_ms);
        let cache = Cache::new(backend, rume_data::cache::config::CacheConfig::default());
        let quota = Arc::new(InMemoryTenantQuotaStore::new());
        let tenant_cache = Arc::new(
            TenantScopedCache::new(cache)
                .with_max_keys_per_tenant(1)
                .with_quota_store(Arc::clone(&quota) as _),
        );

        // Reserve a brand-new key K (`NewlyReserved`) with a 10s TTL at t=0, but delay its
        // backend publication for 20s while L steals the expired slot at t=15s.
        write_delay_ms.store(20_000, std::sync::atomic::Ordering::SeqCst);
        let first_write = {
            let c = Arc::clone(&tenant_cache);
            async move {
                c.set("t1", "k-new", &"v1", Some(Duration::from_secs(10)))
                    .await
            }
        };
        let steal = {
            let c = Arc::clone(&tenant_cache);
            let delay = Arc::clone(&write_delay_ms);
            async move {
                tokio::time::sleep(Duration::from_secs(15)).await;
                delay.store(0, std::sync::atomic::Ordering::SeqCst);
                c.set("t1", "l-steal", &"vl", Some(Duration::from_secs(60)))
                    .await
            }
        };
        let (first_res, steal_res) = tokio::join!(first_write, steal);

        steal_res.expect("L takes the slot after K's initial reservation expired");
        let err =
            first_res.expect_err("NewlyReserved K must fail finalization when slot was stolen");
        assert!(matches!(err, CacheError::QuotaExceeded(_)), "got {err:?}");
        assert_eq!(
            tenant_cache
                .get::<String>("t1", "k-new")
                .await
                .expect("get"),
            None,
            "untracked NewlyReserved key must be evicted on finalization rejection"
        );
        assert_eq!(
            tenant_cache.active_key_count("t1").await.expect("count"),
            1,
            "only L remains tracked"
        );
    }

    #[tokio::test]
    async fn serialization_failure_never_reserves_or_evicts_peer_quota_slot() {
        struct FailingSerialize;
        impl Serialize for FailingSerialize {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("intentional serialize error"))
            }
        }

        let cache = Cache::new(
            rume_data::cache::backend::InMemoryCacheBackend::new(),
            rume_data::cache::config::CacheConfig::default(),
        );
        let quota = Arc::new(InMemoryTenantQuotaStore::new());
        let tenant_cache = TenantScopedCache::new(cache)
            .with_max_keys_per_tenant(1)
            .with_quota_store(Arc::clone(&quota) as _);

        tenant_cache
            .set("t1", "shared-k", &"good", Some(Duration::from_secs(60)))
            .await
            .expect("initial write succeeds");
        let err = tenant_cache
            .set(
                "t1",
                "shared-k",
                &FailingSerialize,
                Some(Duration::from_secs(60)),
            )
            .await
            .expect_err("failing serialization must return Serialization error");
        assert!(matches!(err, CacheError::Serialization(_)));
        assert_eq!(
            tenant_cache.active_key_count("t1").await.expect("count"),
            1,
            "failed serialization must never evict an active reservation for the same key"
        );
    }
}
