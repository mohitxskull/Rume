//! Configuration loading for Rume applications.
//!
//! Configuration is kept declarative: projects use TOML files under `config/`,
//! validated into ordinary Rust structs at boot.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::Format;
use figment::providers::Toml;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// A convenience alias for configuration loading results.
pub type Result<T> = std::result::Result<T, ConfigError>;
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no configuration file found in {dir}")]
    NotFound { dir: PathBuf },

    #[error("failed to validate configuration: {0}")]
    Validation(String),
}

impl From<figment::Error> for ConfigError {
    fn from(err: figment::Error) -> Self {
        let redacted = crate::env::secrets::redact_diagnostic_text(&err.to_string(), &[]);
        Self::Validation(redacted)
    }
}

/// Load and validate a typed configuration from the current directory.
pub fn load<T>() -> Result<T>
where
    T: DeserializeOwned,
{
    load_in(Path::new("."))
}

/// Load and validate a typed configuration rooted at `dir`.
pub fn load_in<T>(dir: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let figment = build_figment(dir)?;
    figment.extract::<T>().map_err(ConfigError::from)
}

/// Load the merged configuration as a raw TOML value.
pub fn load_raw(dir: &Path) -> Result<toml::Value> {
    let figment = build_figment(dir)?;
    figment.extract::<toml::Value>().map_err(ConfigError::from)
}

/// Load the merged configuration as a pretty TOML string.
pub fn load_raw_pretty(dir: &Path) -> Result<String> {
    let value = load_raw(dir)?;
    toml::to_string_pretty(&value).map_err(|err| ConfigError::Validation(err.to_string()))
}

fn build_figment(dir: &Path) -> Result<Figment> {
    let env_name = crate::env::resolve_env_name();

    let candidates = [
        dir.join("config").join("app.toml"),
        dir.join("config").join(format!("app.{env_name}.toml")),
        dir.join("config").join("app.local.toml"),
        dir.join("config")
            .join(format!("app.{env_name}.local.toml")),
    ];

    let mut figment = Figment::new();
    let mut found = false;

    for path in candidates {
        if path.is_file() {
            figment = figment.merge(Toml::file(&path));
            found = true;
        }
    }

    if !found {
        return Err(ConfigError::NotFound {
            dir: dir.to_path_buf(),
        });
    }

    Ok(figment)
}

/// Generate a JSON Schema for a configuration type.
///
/// Enable the `schemars` feature on `rume` to use this.
#[cfg(feature = "schemars")]
pub fn schema<T>() -> Result<String>
where
    T: schemars::JsonSchema,
{
    let schema = schemars::schema_for!(T);
    serde_json::to_string_pretty(&schema).map_err(|err| ConfigError::Validation(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_config(dir: &Path, name: &str, content: &str) {
        let path = dir.join("config").join(name);
        fs::create_dir_all(dir.join("config")).ok();
        fs::write(&path, content).ok();
    }

    #[test]
    fn load_raw_not_found_when_no_config() {
        let dir = std::env::temp_dir().join("rume_config_test_not_found");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).ok();

        let result = load_raw(&dir);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::NotFound { dir: found_dir } => {
                assert_eq!(found_dir, dir);
            }
            _ => panic!("expected ConfigError::NotFound"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_raw_reads_app_toml() {
        let dir = std::env::temp_dir().join("rume_config_test_app_toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).ok();

        write_config(&dir, "app.toml", "app_name = \"test\"\nport = 3000\n");

        let result = load_raw(&dir);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value.get("app_name").and_then(|v| v.as_str()), Some("test"));
        assert_eq!(value.get("port").and_then(|v| v.as_integer()), Some(3000));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_raw_pretty_returns_valid_toml() {
        let dir = std::env::temp_dir().join("rume_config_test_pretty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).ok();

        write_config(&dir, "app.toml", "key = \"value\"\n");

        let result = load_raw_pretty(&dir);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("key"));
        assert!(text.contains("value"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_raw_local_toml_is_loaded() {
        let dir = std::env::temp_dir().join("rume_config_test_local");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config")).ok();

        write_config(&dir, "app.toml", "key = \"base\"\n");
        write_config(&dir, "app.local.toml", "key = \"local\"\n");

        let result = load_raw(&dir);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value.get("key").and_then(|v| v.as_str()), Some("local"));

        let _ = fs::remove_dir_all(&dir);
    }
}
