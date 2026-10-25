//! Environment loading for Rume applications.
//!
//! Rume does not use a runtime schema DSL for environment variables. Instead,
//! a project declares its environment as an ordinary Rust struct deriving
//! `serde::Deserialize`, and [`load`] validates it at boot using `figment`.
//!
//! ```ignore
//! #[derive(Debug, serde::Deserialize)]
//! #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
//! struct Env {
//!     pub host: std::net::IpAddr,
//!     pub port: u16,
//!     pub app_key: secrecy::SecretBox<String>,
//! }
//!
//! let env: Env = rume::env::load()?;
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::Serialized;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// A resolved environment: the merged values plus where each one came from.
#[derive(Debug, Clone, Default)]
pub struct ResolvedEnv {
    /// Resolved variable values.
    pub values: BTreeMap<String, String>,
    /// Provenance for each resolved value.
    pub sources: BTreeMap<String, String>,
}

impl ResolvedEnv {
    /// Load the environment for the current working directory.
    pub fn load() -> Result<Self> {
        Self::load_in(Path::new("."))
    }

    /// Load the environment rooted at `dir`.
    pub fn load_in(dir: &Path) -> Result<Self> {
        load_merged_map_in(dir)
    }

    /// Look up a resolved value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

/// A convenience alias for environment loading results.
pub type Result<T> = std::result::Result<T, EnvError>;
#[derive(Debug, Error)]
pub enum EnvError {
    #[error("I/O error while reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse environment file {path}: {source}")]
    Dotenv {
        path: PathBuf,
        #[source]
        source: dotenvy::Error,
    },

    #[error("failed to validate environment: {0}")]
    Validation(String),
}

impl EnvError {
    fn from_figment(err: figment::Error) -> Self {
        Self::Validation(err.to_string())
    }
}

impl From<figment::Error> for EnvError {
    fn from(err: figment::Error) -> Self {
        Self::Validation(err.to_string())
    }
}

/// Resolve the active environment name.
///
/// Precedence: `RUME_ENV`, then `RAILS_ENV`, then `NODE_ENV`, then
/// `"development"`.
pub fn resolve_env_name() -> String {
    for var in ["RUME_ENV", "RAILS_ENV", "NODE_ENV"] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
        {
            return value;
        }
    }
    "development".to_string()
}

/// Load and validate a typed environment from the current directory.
pub fn load<T>() -> Result<T>
where
    T: DeserializeOwned,
{
    load_in(Path::new("."))
}

/// Load and validate a typed environment rooted at `dir`.
pub fn load_in<T>(dir: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let resolved = load_merged_map_in(dir)?;

    // Parse scalar values the same way figment's built-in `Env` provider does,
    // so numbers and booleans are not treated as strings.
    let mut parsed: BTreeMap<String, figment::value::Value> = BTreeMap::new();
    for (key, value) in resolved.values {
        let value = value
            .parse::<figment::value::Value>()
            .map_err(|err| EnvError::Validation(err.to_string()))?;
        parsed.insert(key, value);
    }

    let figment = Figment::from(Serialized::defaults(parsed));
    figment.extract::<T>().map_err(EnvError::from_figment)
}

/// Load the merged environment map for the current directory.
pub fn load_merged_map() -> Result<ResolvedEnv> {
    load_merged_map_in(Path::new("."))
}

fn load_merged_map_in(dir: &Path) -> Result<ResolvedEnv> {
    let env_name = resolve_env_name();

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();

    for (path, label) in dotenv_candidates(dir, &env_name) {
        if !path.is_file() {
            continue;
        }

        let iterator = dotenvy::from_path_iter(&path).map_err(|source| EnvError::Dotenv {
            path: path.clone(),
            source,
        })?;

        for item in iterator {
            let (key, raw_value) = item.map_err(|source| EnvError::Dotenv {
                path: path.clone(),
                source,
            })?;

            let value = resolve_value(raw_value, dir).map_err(|source| EnvError::Io {
                path: path.clone(),
                source,
            })?;

            values.insert(key.clone(), value);
            sources.insert(key, label.clone());
        }
    }

    // Process environment always wins.
    for (key, value) in std::env::vars() {
        values.insert(key.clone(), value);
        sources.insert(key, "process env".to_string());
    }

    Ok(ResolvedEnv { values, sources })
}

/// Candidate files, from lowest to highest priority.
fn dotenv_candidates(dir: &Path, env_name: &str) -> Vec<(PathBuf, String)> {
    let is_test = env_name == "test" || env_name == "testing";

    let mut candidates = vec![
        (dir.join(".env"), ".env".to_string()),
        (
            dir.join(format!(".env.{env_name}")),
            format!(".env.{env_name}"),
        ),
    ];

    if !is_test {
        candidates.push((dir.join(".env.local"), ".env.local".to_string()));
        candidates.push((
            dir.join(format!(".env.{env_name}.local")),
            format!(".env.{env_name}.local"),
        ));
    }

    // Lowest to highest priority.
    candidates
}

fn resolve_value(value: String, dir: &Path) -> std::io::Result<String> {
    // Escaped literal: "file\:not-a-path" -> "file:not-a-path".
    if let Some(rest) = value.strip_prefix("file\\:") {
        return Ok(format!("file:{rest}"));
    }

    if let Some(path) = value.strip_prefix("file:") {
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            dir.join(path)
        };

        let contents = fs::read_to_string(&full)?;
        // Environment values conventionally do not carry a trailing newline.
        Ok(contents
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string())
    } else {
        Ok(value)
    }
}

/// Heuristic: is this key likely to hold a secret?
pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("TOKEN")
        || upper.ends_with("_KEY")
        || upper == "KEY"
}

/// Heuristic: does this value look like a URL containing credentials?
pub fn value_looks_sensitive(value: &str) -> bool {
    if let Some(rest) = value.split("://").nth(1)
        && let Some(authority) = rest.split('/').next()
    {
        return authority.contains('@') && authority.split('@').next().unwrap_or("").contains(':');
    }
    false
}

/// Redact a value for display when appropriate.
pub fn redact(key: &str, value: &str) -> String {
    if is_secret_key(key) || value_looks_sensitive(value) {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_keys() {
        assert_eq!(redact("APP_KEY", "secret"), "[redacted]");
        assert_eq!(redact("PASSWORD", "x"), "[redacted]");
        assert_eq!(redact("API_TOKEN", "x"), "[redacted]");
        assert_eq!(redact("HOST", "localhost"), "localhost");
    }

    #[test]
    fn redacts_credential_urls() {
        assert_eq!(
            redact("DATABASE_URL", "postgres://user:pass@host/db"),
            "[redacted]"
        );
        assert_eq!(
            redact("DATABASE_URL", "postgres://host/db"),
            "postgres://host/db"
        );
    }

    #[test]
    fn detects_secret_keys() {
        assert!(is_secret_key("APP_KEY"));
        assert!(is_secret_key("API_TOKEN"));
        assert!(is_secret_key("DB_PASSWORD"));
        assert!(!is_secret_key("HOST"));
        assert!(!is_secret_key("PORT"));
    }
}
