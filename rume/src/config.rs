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

/// Errors produced while loading configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no configuration file found in {dir}")]
    NotFound { dir: PathBuf },

    #[error("failed to validate configuration: {0}")]
    Validation(String),
}

impl From<figment::Error> for ConfigError {
    fn from(err: figment::Error) -> Self {
        Self::Validation(err.to_string())
    }
}

/// Load and validate a typed configuration from the current directory.
pub fn load<T>() -> Result<T, ConfigError>
where
    T: DeserializeOwned,
{
    load_in(Path::new("."))
}

/// Load and validate a typed configuration rooted at `dir`.
pub fn load_in<T>(dir: &Path) -> Result<T, ConfigError>
where
    T: DeserializeOwned,
{
    let figment = build_figment(dir)?;
    figment.extract::<T>().map_err(ConfigError::from)
}

/// Load the merged configuration as a raw TOML value.
pub fn load_raw(dir: &Path) -> Result<toml::Value, ConfigError> {
    let figment = build_figment(dir)?;
    figment.extract::<toml::Value>().map_err(ConfigError::from)
}

/// Load the merged configuration as a pretty TOML string.
pub fn load_raw_pretty(dir: &Path) -> Result<String, ConfigError> {
    let value = load_raw(dir)?;
    toml::to_string_pretty(&value).map_err(|err| ConfigError::Validation(err.to_string()))
}

fn build_figment(dir: &Path) -> Result<Figment, ConfigError> {
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
