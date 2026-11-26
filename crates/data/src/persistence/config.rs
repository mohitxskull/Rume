//! Database and connection pool configuration.

use serde::{Deserialize, Serialize};
use std::time::Duration;

fn default_min_connections() -> u32 {
    1
}

fn default_max_connections() -> u32 {
    10
}

fn default_connect_timeout_secs() -> u64 {
    5
}

/// Configuration settings for database connection pools and migrations.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Connection URL or connection string.
    pub url: String,

    /// Minimum number of idle connections to maintain in the pool.
    #[serde(default = "default_min_connections")]
    pub min_connections: u32,

    /// Maximum total number of connections allowed in the pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,

    /// Connection acquisition timeout in seconds.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,

    /// Maximum idle duration before an unused connection is retired.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,

    /// Whether to automatically execute pending migrations on application boot.
    #[serde(default)]
    pub auto_migrate: bool,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_url = rume_foundation::env::secrets::redact_credential_urls_in_text(&self.url);
        f.debug_struct("DatabaseConfig")
            .field("url", &redacted_url)
            .field("min_connections", &self.min_connections)
            .field("max_connections", &self.max_connections)
            .field("connect_timeout_secs", &self.connect_timeout_secs)
            .field("idle_timeout_secs", &self.idle_timeout_secs)
            .field("auto_migrate", &self.auto_migrate)
            .finish()
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "memory://rume".to_string(),
            min_connections: default_min_connections(),
            max_connections: default_max_connections(),
            connect_timeout_secs: default_connect_timeout_secs(),
            idle_timeout_secs: None,
            auto_migrate: false,
        }
    }
}

impl DatabaseConfig {
    /// Create a new configuration with the specified database URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }

    /// Set minimum connections.
    pub fn with_min_connections(mut self, min: u32) -> Self {
        self.min_connections = min;
        self
    }

    /// Set maximum connections.
    pub fn with_max_connections(mut self, max: u32) -> Self {
        self.max_connections = max;
        self
    }

    /// Set connection timeout in seconds.
    pub fn with_connect_timeout(mut self, seconds: u64) -> Self {
        self.connect_timeout_secs = seconds;
        self
    }

    /// Enable or disable auto-migration.
    pub fn with_auto_migrate(mut self, auto: bool) -> Self {
        self.auto_migrate = auto;
        self
    }

    /// Return connection timeout as a `Duration`.
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// Return idle timeout as an optional `Duration`.
    pub fn idle_timeout(&self) -> Option<Duration> {
        self.idle_timeout_secs.map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_config_debug_redacts_url_credentials() {
        let cfg = DatabaseConfig::new("postgres://admin:super_secret_pw@db.internal:5432/app");
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("[redacted]"));
        assert!(!dbg.contains("super_secret_pw"));
    }
}
