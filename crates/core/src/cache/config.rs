//! Cache configuration settings.

use serde::{Deserialize, Serialize};
use std::time::Duration;

fn default_default_ttl_secs() -> u64 {
    300 // 5 minutes
}

/// Cache backend and driver configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Connection string or URL (e.g. `memory://` or `redis://localhost:6379`).
    pub url: String,

    /// Default time-to-live for cached values in seconds when unspecified.
    #[serde(default = "default_default_ttl_secs")]
    pub default_ttl_secs: u64,

    /// Optional namespace prefix prepended to all cache keys (e.g. "rume:app:").
    #[serde(default)]
    pub prefix: Option<String>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            url: "memory://".to_string(),
            default_ttl_secs: default_default_ttl_secs(),
            prefix: None,
        }
    }
}

impl CacheConfig {
    /// Creates a new configuration with the given URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }

    /// Sets default TTL in seconds.
    pub fn with_default_ttl(mut self, secs: u64) -> Self {
        self.default_ttl_secs = secs;
        self
    }

    /// Sets key prefix namespace.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Returns default TTL as a `Duration`.
    pub fn default_ttl(&self) -> Duration {
        Duration::from_secs(self.default_ttl_secs)
    }
}
