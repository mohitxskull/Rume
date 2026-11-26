//! Async and Redis-backed SAML 2.0 assertion replay protection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::verify::{SamlError, SamlReplayCache};

/// Async replay-protection cache for consumed SAML assertion IDs.
pub trait AsyncSamlReplayCache: Send + Sync {
    /// Maximum clock skew (in seconds) that validators sharing this cache may configure.
    fn max_clock_skew_secs(&self) -> u64 {
        300
    }

    /// Atomically checks expiry and records `assertion_id` if absent.
    fn record_if_absent_async<'a>(
        &'a self,
        assertion_id: &'a str,
        expires_at: u64,
        now_epoch_sec: u64,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SamlError>> + Send + 'a>>;
}

/// Adapts any synchronous [`SamlReplayCache`] to [`AsyncSamlReplayCache`].
#[derive(Debug, Clone)]
pub struct SyncReplayCacheAdapter<T: SamlReplayCache> {
    inner: Arc<T>,
}

impl<T: SamlReplayCache> SyncReplayCacheAdapter<T> {
    /// Wraps a synchronous [`SamlReplayCache`].
    pub fn new(inner: Arc<T>) -> Self {
        Self { inner }
    }
}

impl<T: SamlReplayCache + 'static> AsyncSamlReplayCache for SyncReplayCacheAdapter<T> {
    fn max_clock_skew_secs(&self) -> u64 {
        self.inner.max_clock_skew_secs()
    }

    fn record_if_absent_async<'a>(
        &'a self,
        assertion_id: &'a str,
        expires_at: u64,
        now_epoch_sec: u64,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SamlError>> + Send + 'a>> {
        Box::pin(async move {
            self.inner
                .record_if_absent(assertion_id, expires_at, now_epoch_sec)
        })
    }
}

/// Distributed Redis-backed [`AsyncSamlReplayCache`] using atomic `SET NX EX`.
#[cfg(feature = "redis")]
#[derive(Debug, Clone)]
pub struct RedisSamlReplayCache {
    pool: deadpool_redis::Pool,
    key_prefix: String,
    max_clock_skew_secs: u64,
}

#[cfg(feature = "redis")]
impl RedisSamlReplayCache {
    /// Creates a new [`RedisSamlReplayCache`] with default prefix `"rume:saml:replay:"` and 300s skew.
    pub fn new(pool: deadpool_redis::Pool) -> Self {
        Self {
            pool,
            key_prefix: "rume:saml:replay:".to_string(),
            max_clock_skew_secs: 300,
        }
    }

    /// Sets a custom Redis key prefix.
    pub fn with_key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Sets the maximum clock skew in seconds.
    pub fn with_max_clock_skew_secs(mut self, secs: u64) -> Self {
        self.max_clock_skew_secs = secs;
        self
    }

    /// Formats the Redis key for `assertion_id`.
    pub fn format_key(&self, assertion_id: &str) -> String {
        format!("{}{}", self.key_prefix, assertion_id)
    }
}

#[cfg(feature = "redis")]
impl AsyncSamlReplayCache for RedisSamlReplayCache {
    fn max_clock_skew_secs(&self) -> u64 {
        self.max_clock_skew_secs
    }

    fn record_if_absent_async<'a>(
        &'a self,
        assertion_id: &'a str,
        expires_at: u64,
        now_epoch_sec: u64,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SamlError>> + Send + 'a>> {
        Box::pin(async move {
            if now_epoch_sec >= expires_at {
                return Err(SamlError::Expired {
                    not_on_or_after: expires_at,
                    current: now_epoch_sec,
                });
            }

            let ttl_secs = (expires_at - now_epoch_sec).max(1);
            let key = self.format_key(assertion_id);
            let mut conn = self.pool.get().await.map_err(|e| {
                SamlError::Malformed(format!("redis replay cache connection failed: {e}"))
            })?;

            let res: Option<String> = deadpool_redis::redis::cmd("SET")
                .arg(&key)
                .arg("1")
                .arg("NX")
                .arg("EX")
                .arg(ttl_secs)
                .query_async(&mut conn)
                .await
                .map_err(|e| {
                    SamlError::Malformed(format!("redis replay cache SET NX EX failed: {e}"))
                })?;

            Ok(res.is_some())
        })
    }
}
