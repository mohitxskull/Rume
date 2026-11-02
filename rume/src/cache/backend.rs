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

    /// Evicts all cached entries associated with a specific tag.
    fn invalidate_tag(&self, tag: &str) -> BoxFuture<'_, Result<usize, CacheError>>;

    /// Evicts all entries from the cache.
    fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>>;

    /// Verifies cache connectivity.
    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>>;
}
