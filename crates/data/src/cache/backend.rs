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

    /// Verifies cache connectivity.
    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>>;
}

type CacheMap = std::sync::Arc<
    tokio::sync::RwLock<std::collections::HashMap<String, (Vec<u8>, Option<std::time::Instant>)>>,
>;
type TagMap = std::sync::Arc<
    tokio::sync::RwLock<std::collections::HashMap<String, std::collections::HashSet<String>>>,
>;

/// Thread-safe in-memory cache backend suitable for testing and single-instance deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemoryCacheBackend {
    entries: CacheMap,
    tag_map: TagMap,
}

impl InMemoryCacheBackend {
    /// Creates a new in-memory cache backend.
    pub fn new() -> Self {
        Self::default()
    }

    fn prune_key_from_tags(
        tag_map: &mut std::collections::HashMap<String, std::collections::HashSet<String>>,
        key: &str,
    ) {
        tag_map.retain(|_, keys| {
            keys.remove(key);
            !keys.is_empty()
        });
    }
}

impl CacheBackend for InMemoryCacheBackend {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let now = std::time::Instant::now();
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
                // `tag_map` (acquiring `tag_map` before `entries` to preserve
                // global lock order) so expired keys do not accumulate forever.
                let mut tag_map = self.tag_map.write().await;
                let mut map = self.entries.write().await;
                if map
                    .get(&key)
                    .is_some_and(|(_, exp)| exp.is_some_and(|e| std::time::Instant::now() >= e))
                {
                    map.remove(&key);
                    Self::prune_key_from_tags(&mut tag_map, &key);
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
            let now = std::time::Instant::now();
            let expires_at = ttl.map(|t| now + t);
            let mut map = self.entries.write().await;
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
            // Why: acquire `tag_map` before `entries` (matching `invalidate_tag_keys` and
            // `clear` to prevent lock-order inversion) and hold both guards across the
            // synchronous inserts so future cancellation while waiting for a lock can never
            // publish a value without its tag memberships.
            let mut tag_map = self.tag_map.write().await;
            let mut map = self.entries.write().await;
            let expires_at = ttl.map(|t| std::time::Instant::now() + t);
            map.insert(key.clone(), (value, expires_at));
            for tag in tags_vec {
                tag_map.entry(tag).or_default().insert(key.clone());
            }
            Ok(())
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            // Why: acquire `tag_map` before `entries` (global lock order) and
            // prune `key` from all tag sets so deleted tagged keys do not leak
            // memory in `tag_map`.
            let mut tag_map = self.tag_map.write().await;
            let mut map = self.entries.write().await;
            let removed = map.remove(&key).is_some();
            if removed {
                Self::prune_key_from_tags(&mut tag_map, &key);
            }
            Ok(removed)
        })
    }

    fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let now = std::time::Instant::now();
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
                let mut tag_map = self.tag_map.write().await;
                let mut map = self.entries.write().await;
                if map
                    .get(&key)
                    .is_some_and(|(_, exp)| exp.is_some_and(|e| std::time::Instant::now() >= e))
                {
                    map.remove(&key);
                    Self::prune_key_from_tags(&mut tag_map, &key);
                }
            }
            Ok(false)
        })
    }

    fn invalidate_tag_keys(&self, tag: &str) -> BoxFuture<'_, Result<Vec<String>, CacheError>> {
        let tag = tag.to_string();
        Box::pin(async move {
            // Why: take BOTH locks (`tag_map` then `entries`, the order used by `set_with_tags`
            // and `clear`) before removing the tag, with no await between removal and entry
            // deletion. Removing the tag first and then awaiting `entries` let a cancellation
            // in between drop the tag index while leaving its values readable, and a retry
            // would then return zero.
            let mut tag_map = self.tag_map.write().await;
            let mut map = self.entries.write().await;
            let Some(keys) = tag_map.remove(&tag) else {
                return Ok(Vec::new());
            };
            let mut deleted = Vec::new();
            for key in keys {
                if let Some((_, expires_at)) = map.remove(&key) {
                    if let Some(exp) = expires_at
                        && std::time::Instant::now() >= exp
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
            // Lock `tag_map` before `entries` to match `set_with_tags` and `invalidate_tag_keys`.
            let mut tag_map = self.tag_map.write().await;
            let mut map = self.entries.write().await;
            map.clear();
            tag_map.clear();
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
        assert_eq!(backend.tag_map.read().await.len(), 1);
        assert!(backend.delete("k1").await.unwrap());
        assert_eq!(
            backend.tag_map.read().await.len(),
            0,
            "tag_map should be empty after deleting its last key"
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
            backend.tag_map.read().await.len(),
            0,
            "expired key's tag should be pruned on get"
        );
    }
}
