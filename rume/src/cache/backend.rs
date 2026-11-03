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
    tokio::sync::RwLock<
        std::collections::HashMap<String, std::collections::HashSet<String>>,
    >,
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
}

impl CacheBackend for InMemoryCacheBackend {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let map = self.entries.read().await;
            if let Some((val, expires_at)) = map.get(&key) {
                if let Some(exp) = expires_at
                    && std::time::Instant::now() >= *exp
                {
                    return Ok(None);
                }
                Ok(Some(val.clone()))
            } else {
                Ok(None)
            }
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
            let expires_at = ttl.map(|t| std::time::Instant::now() + t);
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
            let expires_at = ttl.map(|t| std::time::Instant::now() + t);
            let mut map = self.entries.write().await;
            map.insert(key.clone(), (value, expires_at));
            drop(map);

            let mut tag_map = self.tag_map.write().await;
            for tag in tags_vec {
                tag_map.entry(tag).or_default().insert(key.clone());
            }
            Ok(())
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let mut map = self.entries.write().await;
            Ok(map.remove(&key).is_some())
        })
    }

    fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let key = key.to_string();
        Box::pin(async move {
            let map = self.entries.read().await;
            if let Some((_, expires_at)) = map.get(&key) {
                if let Some(exp) = expires_at
                    && std::time::Instant::now() >= *exp
                {
                    return Ok(false);
                }
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    fn invalidate_tag_keys(&self, tag: &str) -> BoxFuture<'_, Result<Vec<String>, CacheError>> {
        let tag = tag.to_string();
        Box::pin(async move {
            let mut tag_map = self.tag_map.write().await;
            let Some(keys) = tag_map.remove(&tag) else {
                return Ok(Vec::new());
            };
            let mut map = self.entries.write().await;
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
            let mut map = self.entries.write().await;
            map.clear();
            let mut tag_map = self.tag_map.write().await;
            tag_map.clear();
            Ok(())
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move { Ok(()) })
    }
}
