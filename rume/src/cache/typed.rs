//! Typed cache wrapper, cache-aside pattern, and health check integration.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::cache::backend::{CacheBackend, MemoryCacheBackend};
use crate::cache::config::CacheConfig;
use crate::cache::error::CacheError;
use crate::routing::{HealthCheck, HealthCheckResult, HealthStatus};

/// High-level typed cache interface providing JSON serialization, cache-aside patterns, and tag invalidation.
pub struct Cache<B: CacheBackend = MemoryCacheBackend> {
    backend: Arc<B>,
    config: CacheConfig,
    is_closed: Arc<AtomicBool>,
    singleflight: Arc<tokio::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Notify>>>>,
}

impl Cache<MemoryCacheBackend> {
    /// Creates a default in-memory cache instance.
    pub fn memory() -> Self {
        Self::new(MemoryCacheBackend::new(), CacheConfig::default())
    }
}

impl<B: CacheBackend> Clone for Cache<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            config: self.config.clone(),
            is_closed: Arc::clone(&self.is_closed),
            singleflight: Arc::clone(&self.singleflight),
        }
    }
}

impl<B: CacheBackend> Debug for Cache<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("config", &self.config)
            .field("is_closed", &self.is_closed.load(Ordering::Relaxed))
            .finish()
    }
}

impl<B: CacheBackend> Cache<B> {
    /// Creates a new cache instance with the specified backend and configuration.
    pub fn new(backend: B, config: CacheConfig) -> Self {
        Self {
            backend: Arc::new(backend),
            config,
            is_closed: Arc::new(AtomicBool::new(false)),
            singleflight: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    /// Resolves the effective key with configured prefix namespace.
    fn format_key(&self, key: &str) -> String {
        if let Some(prefix) = &self.config.prefix {
            format!("{prefix}{key}")
        } else {
            key.to_string()
        }
    }

    /// Resolves the effective invalidation tag with configured prefix namespace.
    fn format_tag(&self, tag: &str) -> String {
        if let Some(prefix) = &self.config.prefix {
            format!("{prefix}tag:{tag}")
        } else {
            tag.to_string()
        }
    }

    /// Resolves the effective TTL: uses specified duration or falls back to configured default.
    fn resolve_ttl(&self, ttl: Option<Duration>) -> Option<Duration> {
        ttl.or_else(|| Some(self.config.default_ttl()))
    }

    /// Retrieves and deserializes a JSON value by key.
    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }

        let full_key = self.format_key(key);
        let bytes_opt = self.backend.get(&full_key).await?;

        match bytes_opt {
            Some(bytes) => {
                let val: T = serde_json::from_slice(&bytes)
                    .map_err(|e| CacheError::Serialization(e.to_string()))?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }

    /// Serializes and stores a JSON value by key.
    pub async fn set_json<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        self.set_json_with_tags(key, value, &[], ttl).await
    }

    /// Serializes and stores a JSON value associated with invalidation tags.
    pub async fn set_json_with_tags<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        tags: &[&str],
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }

        let full_key = self.format_key(key);
        let formatted_tags: Vec<String> = tags.iter().map(|t| self.format_tag(t)).collect();
        let tag_refs: Vec<&str> = formatted_tags.iter().map(|s| s.as_str()).collect();
        let bytes =
            serde_json::to_vec(value).map_err(|e| CacheError::Serialization(e.to_string()))?;
        let eff_ttl = self.resolve_ttl(ttl);

        self.backend
            .set_with_tags(&full_key, bytes, &tag_refs, eff_ttl)
            .await
    }

    /// Cache-aside pattern: retrieves cached value if present; otherwise runs closure, caches result, and returns.
    pub async fn remember<T, F, Fut>(
        &self,
        key: &str,
        ttl: Option<Duration>,
        f: F,
    ) -> Result<T, CacheError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, CacheError>>,
    {
        self.remember_with_tags(key, &[], ttl, f).await
    }

    /// Cache-aside pattern with tag invalidation support and stampede (thundering herd) protection.
    pub async fn remember_with_tags<T, F, Fut>(
        &self,
        key: &str,
        tags: &[&str],
        ttl: Option<Duration>,
        f: F,
    ) -> Result<T, CacheError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, CacheError>>,
    {
        if let Some(cached) = self.get_json::<T>(key).await? {
            return Ok(cached);
        }

        let full_key = self.format_key(key);

        // Stampede protection: register singleflight slot
        let waiter = {
            let mut flights = self.singleflight.lock().await;
            if let Some(existing) = flights.get(&full_key) {
                Some(Arc::clone(existing))
            } else {
                let notify = Arc::new(tokio::sync::Notify::new());
                flights.insert(full_key.clone(), Arc::clone(&notify));
                None
            }
        };

        if let Some(notify) = waiter {
            // Concurrent request: wait for the in-flight leader to populate cache
            notify.notified().await;
            if let Some(cached) = self.get_json::<T>(key).await? {
                return Ok(cached);
            }
        }

        // Leader execution: compute fresh value
        let result = f().await;

        // Clean up flight slot and notify all awaiting stampede requests
        let to_wake = {
            let mut flights = self.singleflight.lock().await;
            flights.remove(&full_key)
        };
        if let Some(notify) = to_wake {
            notify.notify_waiters();
        }

        match result {
            Ok(fresh) => {
                self.set_json_with_tags(key, &fresh, tags, ttl).await?;
                Ok(fresh)
            }
            Err(e) => Err(e),
        }
    }

    /// Deletes a key from the cache.
    pub async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }
        let full_key = self.format_key(key);
        self.backend.delete(&full_key).await
    }

    /// Checks if a key exists in the cache.
    pub async fn exists(&self, key: &str) -> Result<bool, CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }
        let full_key = self.format_key(key);
        self.backend.exists(&full_key).await
    }

    /// Evicts all cached entries associated with the specified tag.
    pub async fn invalidate_tag(&self, tag: &str) -> Result<usize, CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }
        let full_tag = self.format_tag(tag);
        self.backend.invalidate_tag(&full_tag).await
    }

    /// Clears all entries from the cache.
    pub async fn clear(&self) -> Result<(), CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }
        self.backend.clear().await
    }

    /// Closes the cache interface.
    pub fn close(&self) {
        self.is_closed.store(true, Ordering::SeqCst);
    }

    /// Creates a Rume [`HealthCheck`] for this cache.
    pub fn health_check(&self, name: impl Into<String>) -> CacheHealthCheck<B> {
        CacheHealthCheck {
            name: name.into(),
            cache: self.clone(),
        }
    }
}

/// Health check implementation for cache backends.
pub struct CacheHealthCheck<B: CacheBackend> {
    name: String,
    cache: Cache<B>,
}

impl<B: CacheBackend> Clone for CacheHealthCheck<B> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            cache: self.cache.clone(),
        }
    }
}

impl<B: CacheBackend> Debug for CacheHealthCheck<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheHealthCheck")
            .field("name", &self.name)
            .finish()
    }
}

impl<B: CacheBackend> HealthCheck for CacheHealthCheck<B> {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self) -> Pin<Box<dyn std::future::Future<Output = HealthCheckResult> + Send>> {
        let name = self.name.clone();
        let cache = self.cache.clone();

        Box::pin(async move {
            let mut meta = BTreeMap::new();
            meta.insert(
                "is_closed".to_string(),
                serde_json::json!(cache.is_closed.load(Ordering::Relaxed)),
            );

            if cache.is_closed.load(Ordering::Relaxed) {
                return HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: "cache is closed".to_string(),
                    meta,
                    cached: false,
                };
            }

            match cache.backend.ping().await {
                Ok(()) => HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "cache connection healthy".to_string(),
                    meta,
                    cached: false,
                },
                Err(err) => HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!("cache connection check failed: {err}"),
                    meta,
                    cached: false,
                },
            }
        })
    }
}
