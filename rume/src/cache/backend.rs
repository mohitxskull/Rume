//! Cache backend trait and in-memory implementation with tag invalidation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::Instant;

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

#[derive(Debug, Clone)]
struct CacheEntry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
    tags: BTreeSet<String>,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            Instant::now() >= exp
        } else {
            false
        }
    }
}

#[derive(Debug, Default)]
struct MemoryCacheState {
    entries: BTreeMap<String, CacheEntry>,
    tag_to_keys: BTreeMap<String, BTreeSet<String>>,
    alive: bool,
}

/// An in-memory cache backend with TTL eviction and tag-based invalidation.
#[derive(Debug, Clone)]
pub struct MemoryCacheBackend {
    state: Arc<RwLock<MemoryCacheState>>,
    max_capacity: usize,
}

impl Default for MemoryCacheBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryCacheBackend {
    /// Default maximum cache entry capacity.
    pub const DEFAULT_MAX_CAPACITY: usize = 10_000;

    /// Creates a new in-memory cache backend with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_MAX_CAPACITY)
    }

    /// Creates a new in-memory cache backend with a custom maximum capacity.
    pub fn with_capacity(max_capacity: usize) -> Self {
        Self {
            state: Arc::new(RwLock::new(MemoryCacheState {
                entries: BTreeMap::new(),
                tag_to_keys: BTreeMap::new(),
                alive: true,
            })),
            max_capacity: max_capacity.max(1),
        }
    }

    /// Prunes expired entries and cleans up stale tag references.
    pub fn prune_expired(&self) {
        if let Ok(mut state) = self.state.write() {
            Self::prune_expired_locked(&mut state);
        }
    }

    fn prune_expired_locked(state: &mut MemoryCacheState) {
        let now = Instant::now();
        let mut expired = Vec::new();
        for (k, e) in &state.entries {
            if let Some(exp) = e.expires_at
                && now >= exp
            {
                expired.push((k.clone(), e.tags.clone()));
            }
        }
        for (k, tags) in expired {
            state.entries.remove(&k);
            for tag in tags {
                if let Some(keys) = state.tag_to_keys.get_mut(&tag) {
                    keys.remove(&k);
                }
            }
        }
    }

    /// Toggles the alive status for simulating connection failure in health checks.
    pub fn set_alive(&self, alive: bool) {
        if let Ok(mut state) = self.state.write() {
            state.alive = alive;
        }
    }

    /// Returns the number of active, non-expired cache entries.
    pub fn len(&self) -> usize {
        if let Ok(state) = self.state.read() {
            state.entries.values().filter(|e| !e.is_expired()).count()
        } else {
            0
        }
    }

    /// Returns true if the cache contains no active entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CacheBackend for MemoryCacheBackend {
    fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
        let k = key.to_string();
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !state.alive {
                return Err(CacheError::Connection("cache offline".into()));
            }

            if let Some(entry) = state.entries.get(&k) {
                if entry.is_expired() {
                    Ok(None)
                } else {
                    Ok(Some(entry.value.clone()))
                }
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
        self.set_with_tags(key, value, &[], ttl)
    }

    fn set_with_tags(
        &self,
        key: &str,
        value: Vec<u8>,
        tags: &[&str],
        ttl: Option<Duration>,
    ) -> BoxFuture<'_, Result<(), CacheError>> {
        let k = key.to_string();
        let tags_set: BTreeSet<String> = tags.iter().map(|t| t.to_string()).collect();
        let expires_at = ttl.map(|d| Instant::now() + d);

        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !state.alive {
                return Err(CacheError::Connection("cache offline".into()));
            }

            if state.entries.len() > 100 {
                Self::prune_expired_locked(&mut state);
            }

            if state.entries.len() >= self.max_capacity
                && !state.entries.contains_key(&k)
                && let Some(first_key) = state.entries.keys().next().cloned()
                && let Some(old) = state.entries.remove(&first_key)
            {
                for tag in old.tags {
                    if let Some(keys) = state.tag_to_keys.get_mut(&tag) {
                        keys.remove(&first_key);
                    }
                }
            }

            for tag in &tags_set {
                state
                    .tag_to_keys
                    .entry(tag.clone())
                    .or_default()
                    .insert(k.clone());
            }

            let entry = CacheEntry {
                value,
                expires_at,
                tags: tags_set,
            };

            state.entries.insert(k, entry);
            Ok(())
        })
    }

    fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let k = key.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !state.alive {
                return Err(CacheError::Connection("cache offline".into()));
            }

            if let Some(entry) = state.entries.remove(&k) {
                for tag in entry.tags {
                    if let Some(keys) = state.tag_to_keys.get_mut(&tag) {
                        keys.remove(&k);
                    }
                }
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
        let k = key.to_string();
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !state.alive {
                return Err(CacheError::Connection("cache offline".into()));
            }

            if let Some(entry) = state.entries.get(&k) {
                Ok(!entry.is_expired())
            } else {
                Ok(false)
            }
        })
    }

    fn invalidate_tag(&self, tag: &str) -> BoxFuture<'_, Result<usize, CacheError>> {
        let t = tag.to_string();
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !state.alive {
                return Err(CacheError::Connection("cache offline".into()));
            }

            if let Some(keys) = state.tag_to_keys.remove(&t) {
                let mut count = 0;
                for key in keys {
                    if let Some(entry) = state.entries.remove(&key) {
                        count += 1;
                        for other_tag in entry.tags {
                            if other_tag != t
                                && let Some(other_keys) = state.tag_to_keys.get_mut(&other_tag)
                            {
                                other_keys.remove(&key);
                            }
                        }
                    }
                }
                Ok(count)
            } else {
                Ok(0)
            }
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .write()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if !state.alive {
                return Err(CacheError::Connection("cache offline".into()));
            }

            state.entries.clear();
            state.tag_to_keys.clear();
            Ok(())
        })
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>> {
        Box::pin(async move {
            let state = self
                .state
                .read()
                .map_err(|e| CacheError::Backend(e.to_string()))?;

            if state.alive {
                Ok(())
            } else {
                Err(CacheError::Connection("cache offline".into()))
            }
        })
    }
}
