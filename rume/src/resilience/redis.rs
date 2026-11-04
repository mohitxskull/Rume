//! Production Redis-backed sliding-window rate limiter.
//!
//! Provides:
//! - Multi-instance, cluster-wide sliding-window rate limiting
//! - Atomic Redis Lua scripts with ZADD and ZREMRANGEBYSCORE
//! - Millisecond sliding-window evaluation and accurate retry-after calculation

use std::fmt::Debug;

use deadpool_redis::Pool;

use crate::resilience::error::ResilienceError;
use crate::resilience::rate_limiter::{RateLimitConfig, RateLimitStatus};

/// Cluster-wide sliding-window rate limiter powered by Redis.
#[derive(Clone)]
pub struct RedisRateLimiter {
    pool: Pool,
    config: RateLimitConfig,
    prefix: String,
}

impl Debug for RedisRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisRateLimiter")
            .field("config", &self.config)
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl RedisRateLimiter {
    /// Creates a new Redis rate limiter.
    pub fn new(pool: Pool, config: RateLimitConfig) -> Self {
        Self {
            pool,
            config,
            prefix: "rume:ratelimit:".to_string(),
        }
    }

    /// Configures a custom key prefix.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    /// Evaluates if a request from `key` is within limits across the cluster.
    pub async fn check(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
        let redis_key = format!("{}{}", self.prefix, key);
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| ResilienceError::Backend(e.to_string()))?;

        let window_ms = self.config.window.as_millis() as i64;
        let max_req = self.config.max_requests as i64;
        let rand_id = uuid::Uuid::new_v4().to_string();

        let script = r#"
            local key = KEYS[1]
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local window = tonumber(ARGV[1])
            local max_requests = tonumber(ARGV[2])
            local member = ARGV[3]
            local clear_before = now - window

            redis.call('ZREMRANGEBYSCORE', key, '-inf', clear_before)
            local current_requests = redis.call('ZCARD', key)

            if current_requests < max_requests then
                redis.call('ZADD', key, now, member)
                redis.call('PEXPIRE', key, window)
                return {1, max_requests - current_requests - 1, math.ceil(window / 1000)}
            else
                local oldest = redis.call('ZRANGE', key, 0, 0, 'WITHSCORES')
                local retry_after = 1
                if oldest and #oldest >= 2 then
                    local oldest_score = tonumber(oldest[2])
                    retry_after = math.max(1, math.ceil((oldest_score + window - now) / 1000))
                end
                return {0, 0, retry_after}
            end
        "#;

        let res: (i64, i64, i64) = redis::Script::new(script)
            .key(&redis_key)
            .arg(window_ms)
            .arg(max_req)
            .arg(&rand_id)
            .invoke_async(&mut *conn)
            .await
            .map_err(|e| ResilienceError::Backend(e.to_string()))?;

        if res.0 == 1 {
            Ok(RateLimitStatus {
                limit: self.config.max_requests,
                remaining: res.1 as u32,
                reset_after_secs: res.2 as u64,
            })
        } else {
            Err(ResilienceError::RateLimited {
                retry_after_secs: res.2 as u64,
            })
        }
    }
}
