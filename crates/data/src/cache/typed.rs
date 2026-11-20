//! Typed cache wrapper, cache-aside pattern, and health check integration.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::cache::backend::CacheBackend;
use crate::cache::config::CacheConfig;
use crate::cache::error::CacheError;
use rume_foundation::health::{HealthCheck, HealthCheckResult, HealthStatus};

struct SingleflightState {
    // Why: `tokio::sync::watch` latches completion state (`None` -> `Some(succeeded)`),
    // eliminating the lost-wakeup race of `Notify::notified_owned()` which is not
    // registered on the waiter list until first poll or `Pin::enable()`.
    tx: tokio::sync::watch::Sender<Option<bool>>,
}

/// Why: `std::sync::Mutex` is used (and never held across `.await`) instead of
/// `tokio::sync::Mutex` so [`SingleflightLeaderGuard::drop`] can unconditionally lock and
/// remove the flight slot on error, panic, or future cancellation; `tokio::sync::Mutex::try_lock`
/// in synchronous `Drop` could lose contention and leave followers stranded forever. Revisit if
/// Rust stabilizes async `Drop` or singleflight moves to a lock-free concurrent map.
type SingleflightMap = Arc<std::sync::Mutex<BTreeMap<String, Arc<SingleflightState>>>>;

struct SingleflightLeaderGuard {
    singleflight: SingleflightMap,
    key: String,
    state: Arc<SingleflightState>,
    succeeded: bool,
}

impl SingleflightLeaderGuard {
    fn mark_succeeded(&mut self) {
        self.succeeded = true;
    }
}

impl Drop for SingleflightLeaderGuard {
    fn drop(&mut self) {
        {
            let mut flights = self
                .singleflight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Why: verify `Arc::ptr_eq` before removing so a guard can never evict a successor
            // leader's `SingleflightState` slot if the map entry were ever replaced; notify
            // waiters outside the lock so woken tasks do not immediately contend on `singleflight`.
            if let Some(current) = flights.get(&self.key)
                && Arc::ptr_eq(current, &self.state)
            {
                flights.remove(&self.key);
            }
        }
        let _ = self.state.tx.send(Some(self.succeeded));
    }
}

enum FlightSlot {
    Leader(SingleflightLeaderGuard),
    Follower(tokio::sync::watch::Receiver<Option<bool>>),
}

/// High-level typed cache interface providing JSON serialization, cache-aside patterns, and tag invalidation.
pub struct Cache<B: CacheBackend> {
    backend: Arc<B>,
    config: CacheConfig,
    is_closed: Arc<AtomicBool>,
    singleflight: SingleflightMap,
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
            singleflight: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
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

    /// Returns the configured default time-to-live for cached values.
    pub fn default_ttl(&self) -> Duration {
        self.config.default_ttl()
    }

    /// Resolves the effective TTL: uses specified duration or falls back to configured default.
    pub fn resolve_ttl(&self, ttl: Option<Duration>) -> Option<Duration> {
        ttl.or_else(|| Some(self.default_ttl()))
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

        // Stampede protection: contend for singleflight leadership in a loop so followers woken
        // after a failed, panicked, or cancelled leader re-contend instead of falling through to
        // execute `f` unguarded.
        let mut leader_guard = loop {
            let slot = {
                let mut flights = self
                    .singleflight
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(existing) = flights.get(&full_key) {
                    // Subscribe to the latched `watch` channel while holding `flights`; even if
                    // `SingleflightLeaderGuard` drops before the follower first polls, `watch`
                    // retains `Some(succeeded)` so the follower can never deadlock.
                    FlightSlot::Follower(existing.tx.subscribe())
                } else {
                    let (tx, _rx) = tokio::sync::watch::channel(None);
                    let state = Arc::new(SingleflightState { tx });
                    flights.insert(full_key.clone(), Arc::clone(&state));
                    FlightSlot::Leader(SingleflightLeaderGuard {
                        singleflight: Arc::clone(&self.singleflight),
                        key: full_key.clone(),
                        state,
                        succeeded: false,
                    })
                }
            };

            match slot {
                FlightSlot::Leader(mut guard) => {
                    // Why: always re-check the cache after acquiring leadership so a caller that
                    // saw a miss and paused before locking `singleflight` does not overwrite a
                    // value populated in the meantime.
                    if let Some(cached) = self.get_json::<T>(key).await? {
                        guard.mark_succeeded();
                        return Ok(cached);
                    }
                    break guard;
                }
                FlightSlot::Follower(mut rx) => {
                    let succeeded = rx
                        .wait_for(|v| v.is_some())
                        .await
                        .ok()
                        .and_then(|v| *v)
                        .unwrap_or(false);
                    // Why: only read the backend from followers when the leader actually
                    // published a value. On leader failure/cancellation, skip the follower read
                    // and re-contend so only the next single leader queries the backend instead
                    // of O(N^2) backend reads across failing followers.
                    if succeeded && let Some(cached) = self.get_json::<T>(key).await? {
                        return Ok(cached);
                    }
                }
            }
        };

        // Hold `leader_guard` across both `f().await` and `set_json_with_tags` so followers wake
        // only after the cache write completes (or after error/panic/cancellation cleanup).
        let fresh = f().await?;
        self.set_json_with_tags(key, &fresh, tags, ttl).await?;
        leader_guard.mark_succeeded();
        drop(leader_guard);

        Ok(fresh)
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

    fn strip_prefix(&self, key: &str) -> String {
        if let Some(prefix) = &self.config.prefix {
            key.strip_prefix(prefix).unwrap_or(key).to_string()
        } else {
            key.to_string()
        }
    }

    /// Evicts all cached entries associated with the specified tag and returns the count.
    pub async fn invalidate_tag(&self, tag: &str) -> Result<usize, CacheError> {
        let keys = self.invalidate_tag_keys(tag).await?;
        Ok(keys.len())
    }

    /// Evicts all cached entries associated with the specified tag and returns the deleted keys
    /// (with the cache's prefix stripped).
    pub async fn invalidate_tag_keys(&self, tag: &str) -> Result<Vec<String>, CacheError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(CacheError::Closed);
        }
        let full_tag = self.format_tag(tag);
        let raw_keys = self.backend.invalidate_tag_keys(&full_tag).await?;
        let stripped = raw_keys
            .into_iter()
            .map(|k| self.strip_prefix(&k))
            .collect();
        Ok(stripped)
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
                    duration_ms: None,
                };
            }

            match cache.backend.ping().await {
                Ok(()) => HealthCheckResult {
                    name,
                    status: HealthStatus::Ok,
                    message: "cache connection healthy".to_string(),
                    meta,
                    cached: false,
                    duration_ms: None,
                },
                Err(err) => HealthCheckResult {
                    name,
                    status: HealthStatus::Failed,
                    message: format!("cache connection check failed: {err}"),
                    meta,
                    cached: false,
                    duration_ms: None,
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::backend::{BoxFuture, InMemoryCacheBackend};
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug)]
    struct TestCacheBackend {
        inner: InMemoryCacheBackend,
        get_calls: AtomicUsize,
        get_notify: tokio::sync::Notify,
        fail_next_sets: AtomicUsize,
    }

    impl TestCacheBackend {
        fn new(fail_next_sets: usize) -> Self {
            Self {
                inner: InMemoryCacheBackend::new(),
                get_calls: AtomicUsize::new(0),
                get_notify: tokio::sync::Notify::new(),
                fail_next_sets: AtomicUsize::new(fail_next_sets),
            }
        }

        async fn wait_for_gets(&self, expected: usize) {
            loop {
                let notified = self.get_notify.notified();
                if self.get_calls.load(Ordering::SeqCst) >= expected {
                    return;
                }
                notified.await;
            }
        }
    }

    impl CacheBackend for TestCacheBackend {
        fn get(&self, key: &str) -> BoxFuture<'_, Result<Option<Vec<u8>>, CacheError>> {
            let key = key.to_string();
            Box::pin(async move {
                let res = self.inner.get(&key).await;
                self.get_calls.fetch_add(1, Ordering::SeqCst);
                self.get_notify.notify_waiters();
                res
            })
        }

        fn set(
            &self,
            key: &str,
            value: Vec<u8>,
            ttl: Option<Duration>,
        ) -> BoxFuture<'_, Result<(), CacheError>> {
            self.inner.set(key, value, ttl)
        }

        fn set_with_tags(
            &self,
            key: &str,
            value: Vec<u8>,
            tags: &[&str],
            ttl: Option<Duration>,
        ) -> BoxFuture<'_, Result<(), CacheError>> {
            let should_fail = self
                .fail_next_sets
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    if v > 0 { Some(v - 1) } else { None }
                })
                .is_ok();
            if should_fail {
                return Box::pin(async { Err(CacheError::Backend("write failed".into())) });
            }
            self.inner.set_with_tags(key, value, tags, ttl)
        }

        fn delete(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
            self.inner.delete(key)
        }

        fn exists(&self, key: &str) -> BoxFuture<'_, Result<bool, CacheError>> {
            self.inner.exists(key)
        }

        fn invalidate_tag_keys(&self, tag: &str) -> BoxFuture<'_, Result<Vec<String>, CacheError>> {
            self.inner.invalidate_tag_keys(tag)
        }

        fn clear(&self) -> BoxFuture<'_, Result<(), CacheError>> {
            self.inner.clear()
        }

        fn ping(&self) -> BoxFuture<'_, Result<(), CacheError>> {
            self.inner.ping()
        }
    }

    /// Waits until a follower holds an `OwnedNotified` for `key`'s in-flight slot.
    ///
    /// Why: each follower's `OwnedNotified` owns one clone of the flight's `Arc<Notify>`, so a
    /// strong count above the slot's own reference proves the follower joined the flight
    /// (instead of guessing with scheduler yields) and aborting the leader now exercises the
    /// follower-recovery path.
    async fn wait_for_follower_registration(cache: &Cache<InMemoryCacheBackend>, key: &str) {
        let full_key = cache.format_key(key);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let joined = {
                    let flights = cache.singleflight.lock().unwrap();
                    flights
                        .get(&full_key)
                        .is_some_and(|state| state.tx.receiver_count() > 0)
                };
                if joined {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("follower must join the in-flight slot");
    }

    async fn settle_followers() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn singleflight_leader_cancellation_wakes_follower_and_cleans_slot() {
        let backend = TestCacheBackend::new(0);
        let cache = Arc::new(Cache::new(backend, CacheConfig::new("memory://test")));
        let leader_in_f = Arc::new(tokio::sync::Notify::new());

        let leader_cache = Arc::clone(&cache);
        let leader_signal = Arc::clone(&leader_in_f);
        let leader_handle = tokio::spawn(async move {
            leader_cache
                .remember_with_tags("cancel-key", &[], None, || async move {
                    leader_signal.notify_waiters();
                    std::future::pending::<Result<String, CacheError>>().await
                })
                .await
        });

        leader_in_f.notified().await;

        let follower_runs = Arc::new(AtomicUsize::new(0));
        let follower_cache = Arc::clone(&cache);
        let follower_runs_clone = Arc::clone(&follower_runs);
        let follower_handle = tokio::spawn(async move {
            follower_cache
                .remember_with_tags("cancel-key", &[], None, || async move {
                    follower_runs_clone.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, CacheError>("from-follower".to_string())
                })
                .await
        });

        cache.backend.wait_for_gets(2).await;
        settle_followers().await;

        assert!(
            !cache
                .singleflight
                .try_lock()
                .expect("singleflight uncontended")
                .is_empty(),
            "leader must hold singleflight slot while executing"
        );

        leader_handle.abort();
        let join_err = leader_handle.await.expect_err("leader task cancelled");
        assert!(join_err.is_cancelled());

        let follower_res = tokio::time::timeout(Duration::from_millis(500), follower_handle)
            .await
            .expect("follower must wake up after leader cancellation")
            .expect("follower task must not panic")
            .expect("follower remember must succeed");

        assert_eq!(follower_res, "from-follower");
        assert_eq!(follower_runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.get_json::<String>("cancel-key").await.unwrap(),
            Some("from-follower".to_string())
        );
        assert!(
            cache
                .singleflight
                .try_lock()
                .expect("singleflight uncontended")
                .is_empty(),
            "singleflight slot must be cleaned up after follower completes"
        );
    }

    #[tokio::test]
    async fn singleflight_leader_error_and_panic_clean_slot_without_stampede() {
        // Part A: Leader returns Err -> followers wake, re-contend cleanly (max 1 concurrent f)
        let backend = TestCacheBackend::new(0);
        let cache = Arc::new(Cache::new(backend, CacheConfig::new("memory://test")));
        let leader_in_f = Arc::new(tokio::sync::Notify::new());
        let release_leader = Arc::new(tokio::sync::Notify::new());

        let l_cache = Arc::clone(&cache);
        let l_in_f = Arc::clone(&leader_in_f);
        let l_rel = Arc::clone(&release_leader);
        let leader_err_handle = tokio::spawn(async move {
            l_cache
                .remember_with_tags("err-key", &[], None, || async move {
                    l_in_f.notify_waiters();
                    l_rel.notified().await;
                    Err::<String, CacheError>(CacheError::Backend("upstream failed".into()))
                })
                .await
        });

        leader_in_f.notified().await;

        let active_runs = Arc::new(AtomicUsize::new(0));
        let peak_runs = Arc::new(AtomicUsize::new(0));
        let total_runs = Arc::new(AtomicUsize::new(0));
        let mut follower_handles = Vec::new();
        for _ in 0..5 {
            let f_cache = Arc::clone(&cache);
            let active = Arc::clone(&active_runs);
            let peak = Arc::clone(&peak_runs);
            let total = Arc::clone(&total_runs);
            follower_handles.push(tokio::spawn(async move {
                f_cache
                    .remember_with_tags("err-key", &[], None, || async move {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        total.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, CacheError>("recovered".to_string())
                    })
                    .await
            }));
        }

        cache.backend.wait_for_gets(6).await;
        settle_followers().await;
        release_leader.notify_waiters();

        let leader_err = leader_err_handle
            .await
            .expect("leader task ok")
            .expect_err("leader must return Err");
        assert_eq!(leader_err, CacheError::Backend("upstream failed".into()));

        for h in follower_handles {
            let val = tokio::time::timeout(Duration::from_millis(500), h)
                .await
                .expect("follower must complete after leader error")
                .expect("follower task ok")
                .expect("follower remember ok");
            assert_eq!(val, "recovered");
        }
        assert_eq!(
            peak_runs.load(Ordering::SeqCst),
            1,
            "followers must re-contend through singleflight without concurrent stampede"
        );
        assert_eq!(
            total_runs.load(Ordering::SeqCst),
            1,
            "only one follower should re-execute f after leader error"
        );
        assert!(
            cache
                .singleflight
                .try_lock()
                .expect("singleflight uncontended")
                .is_empty()
        );

        // Part B: Leader panics inside tokio::spawn -> slot cleaned, followers re-contend cleanly
        let panic_backend = TestCacheBackend::new(0);
        let panic_cache = Arc::new(Cache::new(panic_backend, CacheConfig::new("memory://test")));
        let p_in_f = Arc::new(tokio::sync::Notify::new());
        let p_release = Arc::new(tokio::sync::Notify::new());

        let pl_cache = Arc::clone(&panic_cache);
        let pl_in_f = Arc::clone(&p_in_f);
        let pl_rel = Arc::clone(&p_release);
        let leader_panic_handle = tokio::spawn(async move {
            pl_cache
                .remember_with_tags::<String, _, _>("panic-key", &[], None, || async move {
                    pl_in_f.notify_waiters();
                    pl_rel.notified().await;
                    panic!("intentional singleflight leader panic");
                })
                .await
        });

        p_in_f.notified().await;

        let p_active = Arc::new(AtomicUsize::new(0));
        let p_peak = Arc::new(AtomicUsize::new(0));
        let p_total = Arc::new(AtomicUsize::new(0));
        let mut p_followers = Vec::new();
        for _ in 0..5 {
            let f_cache = Arc::clone(&panic_cache);
            let active = Arc::clone(&p_active);
            let peak = Arc::clone(&p_peak);
            let total = Arc::clone(&p_total);
            p_followers.push(tokio::spawn(async move {
                f_cache
                    .remember_with_tags("panic-key", &[], None, || async move {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        total.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, CacheError>("after-panic".to_string())
                    })
                    .await
            }));
        }

        panic_cache.backend.wait_for_gets(6).await;
        settle_followers().await;
        p_release.notify_waiters();

        let join_err = leader_panic_handle
            .await
            .expect_err("leader task must panic");
        assert!(join_err.is_panic());

        for h in p_followers {
            let val = tokio::time::timeout(Duration::from_millis(500), h)
                .await
                .expect("follower must wake after leader panic")
                .expect("follower task ok")
                .expect("follower remember ok");
            assert_eq!(val, "after-panic");
        }
        assert_eq!(p_peak.load(Ordering::SeqCst), 1);
        assert_eq!(p_total.load(Ordering::SeqCst), 1);
        assert!(
            panic_cache
                .singleflight
                .try_lock()
                .expect("singleflight uncontended")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn singleflight_backend_write_error_cleans_slot_and_wakes_follower() {
        let backend = TestCacheBackend::new(1);
        let cache = Arc::new(Cache::new(backend, CacheConfig::new("memory://test")));
        let leader_in_f = Arc::new(tokio::sync::Notify::new());
        let release_leader = Arc::new(tokio::sync::Notify::new());

        let l_cache = Arc::clone(&cache);
        let l_in_f = Arc::clone(&leader_in_f);
        let l_rel = Arc::clone(&release_leader);
        let leader_handle = tokio::spawn(async move {
            l_cache
                .remember_with_tags("write-err-key", &[], None, || async move {
                    l_in_f.notify_waiters();
                    l_rel.notified().await;
                    Ok::<_, CacheError>("v1".to_string())
                })
                .await
        });

        leader_in_f.notified().await;

        let follower_runs = Arc::new(AtomicUsize::new(0));
        let f_cache = Arc::clone(&cache);
        let f_runs = Arc::clone(&follower_runs);
        let follower_handle = tokio::spawn(async move {
            f_cache
                .remember_with_tags("write-err-key", &[], None, || async move {
                    f_runs.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, CacheError>("v2".to_string())
                })
                .await
        });

        cache.backend.wait_for_gets(2).await;
        settle_followers().await;
        release_leader.notify_waiters();

        let leader_err = leader_handle
            .await
            .expect("leader task ok")
            .expect_err("leader must propagate backend write error");
        assert_eq!(leader_err, CacheError::Backend("write failed".into()));

        let follower_val = tokio::time::timeout(Duration::from_millis(500), follower_handle)
            .await
            .expect("follower must wake after leader backend write error")
            .expect("follower task ok")
            .expect("follower remember ok");
        assert_eq!(follower_val, "v2");
        assert_eq!(follower_runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.get_json::<String>("write-err-key").await.unwrap(),
            Some("v2".to_string())
        );
        assert!(
            cache
                .singleflight
                .try_lock()
                .expect("singleflight uncontended")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn singleflight_failing_leaders_do_not_trigger_quadratic_follower_reads() {
        let backend = TestCacheBackend::new(0);
        let cache = Arc::new(Cache::new(backend, CacheConfig::new("memory://test")));
        let callers = 12usize;
        let start_barrier = Arc::new(tokio::sync::Barrier::new(callers));

        let mut handles = Vec::with_capacity(callers);
        for i in 0..callers {
            let c = Arc::clone(&cache);
            let b = Arc::clone(&start_barrier);
            handles.push(tokio::spawn(async move {
                b.wait().await;
                c.remember_with_tags::<String, _, _>("all-fail-key", &[], None, || async move {
                    tokio::task::yield_now().await;
                    Err(CacheError::Backend(format!("boom-{i}")))
                })
                .await
            }));
        }

        for h in handles {
            assert!(h.await.expect("task ok").is_err());
        }

        // Each caller does at most 2 reads (initial miss + leader recheck when promoted),
        // never O(N^2) post-wakeup follower reads.
        let total_gets = cache.backend.get_calls.load(Ordering::SeqCst);
        assert!(
            total_gets <= callers * 2,
            "expected linear <= {} backend reads across {callers} failing callers, got {total_gets}",
            callers * 2
        );
    }

    #[tokio::test]
    async fn singleflight_cancelled_tagged_write_recovers_with_intact_tag_invalidation() {
        let backend = InMemoryCacheBackend::new();
        let cache = Arc::new(Cache::new(backend, CacheConfig::new("memory://test")));

        // Cancel a tagged remember_with_tags while the leader is in-flight; a follower
        // recovers and publishes the tagged value, and `invalidate_tag` must evict it.
        let leader_entered = Arc::new(tokio::sync::Notify::new());
        let entered_clone = Arc::clone(&leader_entered);
        let c1 = Arc::clone(&cache);
        let leader = tokio::spawn(async move {
            c1.remember_with_tags("tagged-k", &["group-a"], None, || async move {
                entered_clone.notify_waiters();
                std::future::pending::<Result<String, CacheError>>().await
            })
            .await
        });

        // Why: register the notification BEFORE spawning is impossible here (the leader task
        // is already spawned), so await it under a guard timeout: `notify_waiters` only wakes
        // registered waiters, and a missed signal must fail the test rather than hang it.
        tokio::time::timeout(Duration::from_secs(10), leader_entered.notified())
            .await
            .expect("leader must enter its factory");

        let c2 = Arc::clone(&cache);
        let follower = tokio::spawn(async move {
            c2.remember_with_tags("tagged-k", &["group-a"], None, || async move {
                Ok::<_, CacheError>("recovered".to_string())
            })
            .await
        });

        wait_for_follower_registration(&cache, "tagged-k").await;
        leader.abort();
        let leader_err = leader.await.expect_err("aborted leader must not complete");
        assert!(leader_err.is_cancelled(), "leader must end by cancellation");

        let val = follower
            .await
            .expect("follower task ok")
            .expect("follower recovery ok");
        assert_eq!(val, "recovered");

        let evicted = cache
            .invalidate_tag("group-a")
            .await
            .expect("invalidate_tag ok");
        assert_eq!(
            evicted, 1,
            "recovered value must be registered under its tag and evicted by invalidate_tag"
        );
        assert_eq!(
            cache.get_json::<String>("tagged-k").await.unwrap(),
            None,
            "key must be absent after tag invalidation"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn singleflight_follower_wakes_even_if_leader_completes_before_first_poll() {
        let cache = Cache::new(
            InMemoryCacheBackend::new(),
            CacheConfig::new("memory://watch-test"),
        );
        // Simulate a leader creating a slot, a follower subscribing while the slot
        // is active, and the leader completing BEFORE the follower ever polls `wait_for`.
        let (tx, _rx) = tokio::sync::watch::channel(None);
        let state = Arc::new(SingleflightState { tx });
        let mut follower_rx = state.tx.subscribe();
        let mut guard = SingleflightLeaderGuard {
            singleflight: Arc::clone(&cache.singleflight),
            key: "k".to_string(),
            state: Arc::clone(&state),
            succeeded: false,
        };
        cache
            .singleflight
            .lock()
            .unwrap()
            .insert("k".to_string(), state);
        guard.mark_succeeded();
        drop(guard);

        // Follower polls AFTER leader already dropped — must return immediately, never deadlock.
        let val = tokio::time::timeout(
            Duration::from_millis(50),
            follower_rx.wait_for(|v| v.is_some()),
        )
        .await
        .expect("watch follower must not deadlock when leader finishes before first poll")
        .expect("sender value latched");
        assert_eq!(*val, Some(true));
    }
}
