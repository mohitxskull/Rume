//! Cache backend trait for storage drivers.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::cache::error::CacheError;

/// Pinned boxed future type alias for async cache operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait implemented by storage drivers for cache storage and retrieval.
pub trait CacheBackend: Debug + Send + Sync + 'static {
    /// Retrieves a value by key if present and not expired.
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>>;

    /// Stores a value with an optional time-to-live.
    fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>>;

    /// Stores a value associated with invalidation tags and an optional time-to-live.
    fn set_with_tags(
        &self,
        key: &str,
        value: Vec<u8>,
        tags: &[&str],
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>>;

    /// Deletes a key from the cache.
    fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>>;

    /// Checks if a key exists and has not expired.
    fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>>;

    /// Evicts all cached entries associated with a specific tag and returns the count.
    fn invalidate_tag(&self, tag: &str) -> BoxFuture<'_, Result<usize, CacheError>> {
        let tag = tag.to_string();
        Box::pin(async move {
            let keys = self.invalidate_tag_keys(&tag).await?;
            Ok(keys.len())
        })
    }

    /// Evicts all cached entries associated with a specific tag and returns the deleted keys.
    fn invalidate_tag_keys(&self, tag: &str) -> BoxFuture<'_, Result<Vec<String>, CacheError>>;

    /// Evicts all entries from the cache.
    fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>>;

    /// Evicts all entries whose keys (and associated tag indexes) start with `prefix`.
    ///
    /// Backends that do not support prefix-scoped clearing fall back to [`CacheBackend::clear`].
    fn clear_prefix(&self, _prefix: &str) -> BoxFuture<'_, Result<(), CacheError>> {
        self.clear()
    }

    /// Verifies cache connectivity.
    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>>;
}

type CacheMap = std::sync::Arc<
    tokio::sync::RwLock<std::collections::HashMap<String, (Vec<u8>, Option<tokio::time::Instant>)>>,
>;

#[derive(Debug, Default)]
struct TagIndex {
    tag_to_keys: std::collections::HashMap<String, std::collections::HashSet<String>>,
    key_to_tags: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl TagIndex {
    fn insert(&mut self, tag: String, key: String) {
        self.key_to_tags
            .entry(key.clone())
            .or_default()
            .insert(tag.clone());
        self.tag_to_keys.entry(tag).or_default().insert(key);
    }

    /// Removes `key` from all tag sets it belongs to in O(tags_for_key) time.
    ///
    /// Why: scanning every tag set in `tag_to_keys` on every delete or expiry
    /// eviction would be O(total_tags) under the exclusive write lock.
    fn remove_key(&mut self, key: &str) {
        if let Some(tags) = self.key_to_tags.remove(key) {
            for tag in tags {
                if let Some(keys) = self.tag_to_keys.get_mut(&tag) {
                    keys.remove(key);
                    if keys.is_empty() {
                        self.tag_to_keys.remove(&tag);
                    }
                }
            }
        }
    }

    fn remove_tag(&mut self, tag: &str) -> Option<std::collections::HashSet<String>> {
        let keys = self.tag_to_keys.remove(tag)?;
        for key in &keys {
            self.remove_key(key);
        }
        Some(keys)
    }

    fn clear(&mut self) {
        self.tag_to_keys.clear();
        self.key_to_tags.clear();
    }
}

type TagIndexLock = std::sync::Arc<tokio::sync::RwLock<TagIndex>>;

/// Thread-safe in-memory cache backend suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemoryCacheBackend {
    entries: CacheMap,
    tag_index: TagIndexLock,
}

impl InMemoryCacheBackend {
    /// Creates a new in-memory cache backend.
    pub fn new() -> Self {
        Self::default()
    }
}

impl CacheBackend for InMemoryCacheBackend {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let now = tokio::time::Instant::now();
            let expired = {
                let map = self.entries.read().await;
                if let Some((val, expires_at)) = map.get(&key) {
                    if let Some(exp) = expires_at
                        && now >= *exp
                    {
                        true
                    } else {
                        return Ok(Some(val.clone()));
                    }
                } else {
                    return Ok(None);
                }
            };
            if expired {
                // Why: opportunistically evict expired keys on read and prune
                // `tag_index` (acquiring `tag_index` before `entries` to preserve
                // global lock order) so expired keys do not accumulate forever.
                let mut tag_index = self.tag_index.write().await;
                let mut map = self.entries.write().await;
                if map
                    .get(&key)
                    .is_some_and(|(_, exp)| exp.is_some_and(|e| tokio::time::Instant::now() >= e))
                {
                    map.remove(&key);
                    tag_index.remove_key(&key);
                }
            }
            Ok(None)
        })
    }

    fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            // Why: acquire `tag_index` before `entries` (global lock order) and clear any
            // prior tag memberships on overwrite so stale tag invalidations cannot evict
            // the newly written value (`RC-34`).
            let mut tag_index = self.tag_index.write().await;
            let mut map = self.entries.write().await;
            let now = tokio::time::Instant::now();
            // Why: use `checked_add` so huge TTLs (e.g. `Duration::MAX`) never panic
            // on `Instant` overflow; an unrepresentably distant expiry is treated as no TTL.
            let expires_at = ttl.and_then(|t| now.checked_add(t));
            tag_index.remove_key(&key);
            map.insert(key, (value, expires_at));
            Ok(())
        })
    }

    fn set_with_tags(
        &self,
        key: &str,
        value: Vec<u8>,
        tags: &[&str],
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>> {
        let key = key.to_string();
        let tags_vec: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        Box::pin(async move {
            // Why: acquire `tag_index` before `entries` (matching `invalidate_tag_keys` and
            // `clear` to prevent lock-order inversion), clear prior tag memberships so stale
            // tags never evict the overwritten value (`RC-34`), and hold both guards across
            // the synchronous inserts so cancellation can never publish mismatched state.
            let mut tag_index = self.tag_index.write().await;
            let mut map = self.entries.write().await;
            let now = tokio::time::Instant::now();
            let expires_at = ttl.and_then(|t| now.checked_add(t));
            tag_index.remove_key(&key);
            map.insert(key.clone(), (value, expires_at));
            for tag in tags_vec {
                tag_index.insert(tag, key.clone());
            }
            Ok(())
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            // Why: acquire `tag_index` before `entries` (global lock order) and
            // prune `key` via the reverse index in O(tags_for_key) so deleted
            // tagged keys do not leak memory in `tag_index`.
            let mut tag_index = self.tag_index.write().await;
            let mut map = self.entries.write().await;
            let removed = map.remove(&key).is_some();
            if removed {
                tag_index.remove_key(&key);
            }
            Ok(removed)
        })
    }

    fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let now = tokio::time::Instant::now();
            let expired = {
                let map = self.entries.read().await;
                if let Some((_, expires_at)) = map.get(&key) {
                    if let Some(exp) = expires_at
                        && now >= *exp
                    {
                        true
                    } else {
                        return Ok(true);
                    }
                } else {
                    return Ok(false);
                }
            };
            if expired {
                let mut tag_index = self.tag_index.write().await;
                let mut map = self.entries.write().await;
                if map
                    .get(&key)
                    .is_some_and(|(_, exp)| exp.is_some_and(|e| tokio::time::Instant::now() >= e))
                {
                    map.remove(&key);
                    tag_index.remove_key(&key);
                }
            }
            Ok(false)
        })
    }

    fn invalidate_tag_keys(&self, tag: &str) -> BoxFuture<'_, Result<Vec<String>, CacheError>> {
        let tag = tag.to_string();
        Box::pin(async move {
            // Why: take BOTH locks (`tag_index` then `entries`, the order used by `set_with_tags`
            // and `clear`) before removing the tag, with no await between removal and entry
            // deletion. Removing the tag first and then awaiting `entries` let a cancellation
            // in between drop the tag index while leaving its values readable, and a retry
            // would then return zero.
            let mut tag_index = self.tag_index.write().await;
            let mut map = self.entries.write().await;
            let Some(keys) = tag_index.remove_tag(&tag) else {
                return Ok(Vec::new());
            };
            let mut deleted = Vec::new();
            for key in keys {
                if let Some((_, expires_at)) = map.remove(&key) {
                    if let Some(exp) = expires_at
                        && tokio::time::Instant::now() >= exp
                    {
                        continue;
                    }
                    deleted.push(key);
                }
            }
            Ok(deleted)
        })
    }

    fn invalidate_tag(&self, tag: &str) -> BoxFuture<'_, Result<usize, CacheError>> {
        let tag = tag.to_string();
        Box::pin(async move {
            let keys = self.invalidate_tag_keys(&tag).await?;
            Ok(keys.len())
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move {
            // Lock `tag_index` before `entries` to match `set_with_tags` and `invalidate_tag_keys`.
            let mut tag_index = self.tag_index.write().await;
            let mut map = self.entries.write().await;
            map.clear();
            tag_index.clear();
            Ok(())
        })
    }

    fn clear_prefix(&self, prefix: &str) -> BoxFuture<'_, Result<(), CacheError>> {
        let prefix = prefix.to_string();
        Box::pin(async move {
            // Lock `tag_index` before `entries` to match `set_with_tags` and `invalidate_tag_keys`.
            let mut tag_index = self.tag_index.write().await;
            let mut map = self.entries.write().await;
            let keys_to_remove: Vec<String> = map
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            for key in keys_to_remove {
                map.remove(&key);
                tag_index.remove_key(&key);
            }
            let tags_to_remove: Vec<String> = tag_index
                .tag_to_keys
                .keys()
                .filter(|t| t.starts_with(&prefix))
                .cloned()
                .collect();
            for tag in tags_to_remove {
                tag_index.remove_tag(&tag);
            }
            Ok(())
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delete_prunes_tag_map_and_expired_get_evicts_entry() {
        let backend = InMemoryCacheBackend::new();
        backend
            .set_with_tags("k1", b"v1".to_vec(), &["tag_a"], None)
            .await
            .unwrap();
        assert_eq!(backend.tag_index.read().await.tag_to_keys.len(), 1);
        assert_eq!(backend.tag_index.read().await.key_to_tags.len(), 1);
        assert!(backend.delete("k1").await.unwrap());
        assert_eq!(
            backend.tag_index.read().await.tag_to_keys.len(),
            0,
            "tag_to_keys should be empty after deleting its last key"
        );
        assert_eq!(
            backend.tag_index.read().await.key_to_tags.len(),
            0,
            "key_to_tags should be empty after deleting key"
        );

        backend
            .set_with_tags("k2", b"v2".to_vec(), &["tag_b"], Some(Duration::ZERO))
            .await
            .unwrap();
        assert_eq!(backend.get("k2").await.unwrap(), None);
        assert_eq!(
            backend.entries.read().await.len(),
            0,
            "expired key should be evicted on get"
        );
        assert_eq!(
            backend.tag_index.read().await.tag_to_keys.len(),
            0,
            "expired key's tag should be pruned on get"
        );
        assert_eq!(
            backend.tag_index.read().await.key_to_tags.len(),
            0,
            "expired key's reverse index entry should be pruned on get"
        );
    }

    #[tokio::test]
    async fn set_with_duration_max_does_not_panic_on_instant_overflow() {
        let backend = InMemoryCacheBackend::new();
        backend
            .set("k_max", b"v1".to_vec(), Some(Duration::MAX))
            .await
            .unwrap();
        assert_eq!(backend.get("k_max").await.unwrap(), Some(b"v1".to_vec()));

        backend
            .set_with_tags("k_tag_max", b"v2".to_vec(), &["t1"], Some(Duration::MAX))
            .await
            .unwrap();
        assert_eq!(
            backend.get("k_tag_max").await.unwrap(),
            Some(b"v2".to_vec())
        );
    }

    #[tokio::test]
    async fn overwriting_tagged_key_clears_prior_tag_membership() {
        let backend = InMemoryCacheBackend::new();
        backend
            .set_with_tags("k", b"v1".to_vec(), &["old_tag"], None)
            .await
            .unwrap();

        // Overwrite with `set` (no tags) -> invalidating `old_tag` must NOT evict `k`.
        backend.set("k", b"v2".to_vec(), None).await.unwrap();
        assert_eq!(backend.invalidate_tag("old_tag").await.unwrap(), 0);
        assert_eq!(backend.get("k").await.unwrap(), Some(b"v2".to_vec()));

        // Overwrite with `set_with_tags` using `new_tag` -> invalidating `old_tag` must NOT evict,
        // while invalidating `new_tag` evicts `k`.
        backend
            .set_with_tags("k", b"v3".to_vec(), &["tag_1"], None)
            .await
            .unwrap();
        backend
            .set_with_tags("k", b"v4".to_vec(), &["tag_2"], None)
            .await
            .unwrap();
        assert_eq!(backend.invalidate_tag("tag_1").await.unwrap(), 0);
        assert_eq!(backend.get("k").await.unwrap(), Some(b"v4".to_vec()));
        assert_eq!(backend.invalidate_tag("tag_2").await.unwrap(), 1);
        assert_eq!(backend.get("k").await.unwrap(), None);
    }
}
