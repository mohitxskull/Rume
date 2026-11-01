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

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::Serialized;
use secrecy::{ExposeSecret, SecretBox};
use serde::de::DeserializeOwned;
use thiserror::Error;

/// A resolved environment: the merged values plus where each one came from.
///
/// Secret-typed values (detected via [`is_secret_key`]) are stored in
/// [`secrecy::SecretBox`] and only exposed through [`expose_secret`](secrecy::SecretBox::expose_secret)
/// at the final consumption point, never in logs or error responses.
#[derive(Debug, Default)]
pub struct ResolvedEnv {
    /// Resolved non-secret variable values.
    pub values: BTreeMap<String, String>,
    /// Resolved secret variable values, held in [`SecretBox`].
    pub secrets: BTreeMap<String, SecretBox<String>>,
    /// Provenance for each resolved value.
    pub sources: BTreeMap<String, String>,
    /// Keys that originated from an env file (as opposed to process env only).
    pub file_keys: BTreeSet<String>,
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

    /// Look up a resolved value. Secret values are exposed via `expose_secret()`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.secrets
            .get(key)
            .map(|s| s.expose_secret().as_str())
            .or_else(|| self.values.get(key).map(String::as_str))
    }

    /// Look up a resolved secret value, returning the [`SecretBox`] without
    /// exposing the plaintext.
    pub fn get_secret(&self, key: &str) -> Option<&SecretBox<String>> {
        self.secrets.get(key)
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
    // so numbers and booleans are not treated as strings. Secret values are
    // exposed at this final consumption point for deserialization into typed
    // env structs (which may use `secrecy::SecretBox<String>` fields).
    let mut parsed: BTreeMap<String, figment::value::Value> = BTreeMap::new();
    for (key, value) in resolved.values {
        let value = value
            .parse::<figment::value::Value>()
            .map_err(|err| EnvError::Validation(err.to_string()))?;
        parsed.insert(key, value);
    }
    for (key, secret) in &resolved.secrets {
        let value = secret
            .expose_secret()
            .parse::<figment::value::Value>()
            .map_err(|err| EnvError::Validation(err.to_string()))?;
        parsed.insert(key.clone(), value);
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
    let mut secrets: BTreeMap<String, SecretBox<String>> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut file_keys: BTreeSet<String> = BTreeSet::new();

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

            if is_secret_key(&key) {
                secrets.insert(key.clone(), SecretBox::new(Box::new(value)));
            } else {
                values.insert(key.clone(), value);
            }
            sources.insert(key.clone(), label.clone());
            file_keys.insert(key);
        }
    }

    // Process environment always wins.
    for (key, value) in std::env::vars() {
        if is_secret_key(&key) {
            secrets.insert(key.clone(), SecretBox::new(Box::new(value)));
        } else {
            values.insert(key.clone(), value);
        }
        sources.insert(key, "process env".to_string());
    }

    Ok(ResolvedEnv {
        values,
        secrets,
        sources,
        file_keys,
    })
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

    // .env.local is not loaded in test mode (to avoid local dev settings
    // interfering with tests). But .env.{env_name}.local IS loaded.
    if !is_test {
        candidates.push((dir.join(".env.local"), ".env.local".to_string()));
    }
    candidates.push((
        dir.join(format!(".env.{env_name}.local")),
        format!(".env.{env_name}.local"),
    ));

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

        // Resolve canonical path and enforce that it stays within `dir` to
        // prevent path traversal (e.g. `file:../../etc/secrets/app.key`).
        let canonical_dir = match fs::canonicalize(dir) {
            Ok(d) => d,
            Err(_) => dir.to_path_buf(),
        };
        let canonical_full = match fs::canonicalize(&full) {
            Ok(cf) => cf,
            Err(_) => full.clone(),
        };
        if !canonical_full.starts_with(&canonical_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("file: path escapes directory: {path}"),
            ));
        }

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
///
/// Covers common secret suffixes/prefixes (`SECRET`, `PASSWORD`, `TOKEN`,
/// `_KEY`, `_PASS`, `_PWD`, `_CRED`, `_PRIVATE_KEY`, `STRIPE_*`, etc.).
///
/// Why: The original heuristic only matched `SECRET`/`PASSWORD`/`TOKEN`/`_KEY`,
/// so keys like `APP_PASS`, `STRIPE_SECRET_KEY`, or `DB_PWD` would print
/// plaintext. We err on the side of redacting too much rather than too little.
pub fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();

    // Broad substring matches (covers keys with the word anywhere).
    if upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("TOKEN")
        || upper.contains("_PASS")
        || upper.contains("_PWD")
        || upper.contains("_CRED")
        || upper.contains("_KEY")
    {
        return true;
    }

    // Exact matches for standalone names.
    upper == "KEY"
        || upper == "SECRET"
        || upper == "PASSWORD"
        || upper == "PASS"
        || upper == "PWD"
        || upper == "TOKEN"
        || upper.starts_with("STRIPE_")
        || upper.starts_with("PRIVATE_KEY")
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

    #[test]
    fn redact_broadened_catches_more_secret_keys() {
        // S-m1: broadened detection must catch keys the old heuristic missed.
        // These must be redacted, not printed as plaintext.
        assert_eq!(redact("APP_PASS", "s3cret"), "[redacted]");
        assert_eq!(redact("DB_PWD", "s3cret"), "[redacted]");
        assert_eq!(redact("STRIPE_SECRET_KEY", "sk_live_xxx"), "[redacted]");
        assert_eq!(redact("APP_CRED", "s3cret"), "[redacted]");
        assert_eq!(redact("MY_SECRET_VALUE", "s3cret"), "[redacted]");
        assert_eq!(redact("ACCESS_TOKEN", "tok123"), "[redacted]");
        // Non-secret keys must not be redacted.
        assert_eq!(redact("HOST", "localhost"), "localhost");
        assert_eq!(redact("PORT", "3000"), "3000");
    }

    #[test]
    fn secret_keys_stored_in_secrets_map_not_values_map() {
        // S-M2: secret keys must be routed to the secrets map (SecretBox),
        // not the plain values map, to avoid plaintext exposure.
        let dir = std::env::temp_dir().join("rume_secret_map_test");
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join(".env");
        std::fs::write(&env_file, "APP_KEY=super-secret-value\nHOST=localhost\n").unwrap();

        let resolved = load_merged_map_in(&dir).unwrap();

        assert!(!resolved.values.contains_key("APP_KEY"));
        assert!(resolved.secrets.contains_key("APP_KEY"));
        assert_eq!(resolved.values.get("HOST"), Some(&"localhost".to_string()));
        assert!(!resolved.secrets.contains_key("HOST"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn get_exposes_secret_via_expose_secret() {
        // S-M2: get() exposes secret plaintext at consumption point.
        let dir = std::env::temp_dir().join("rume_secret_get_test");
        std::fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join(".env");
        std::fs::write(&env_file, "APP_KEY=super-secret-value\n").unwrap();

        let resolved = load_merged_map_in(&dir).unwrap();

        assert_eq!(resolved.get("APP_KEY").unwrap(), "super-secret-value");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_value_traversal_is_rejected() {
        let dir = std::env::temp_dir();
        assert!(resolve_value("file:../../etc/secrets/app.key".to_string(), &dir).is_err());
    }

    #[test]
    fn file_value_absolute_path_is_rejected() {
        assert!(resolve_value("file:/etc/passwd".to_string(), &std::env::temp_dir()).is_err());
    }

    #[test]
    fn file_value_normal_path_works() {
        let dir = std::env::temp_dir();
        let file_path = dir.join("rume_env_test_file.txt");
        std::fs::write(&file_path, "test content").unwrap();
        let result = resolve_value(format!("file:{}", file_path.display()), &dir);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test content");
        std::fs::remove_file(&file_path).ok();
    }
}
