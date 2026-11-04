//! Configuration loading for Rume applications.
//!
//! Configuration is kept declarative: projects use TOML files under `config/`,
//! validated into ordinary Rust structs at boot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::Format;
use figment::providers::Toml;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A convenience alias for configuration loading results.
pub type Result<T> = std::result::Result<T, ConfigError>;

/// Detailed explanation for a single configuration key's provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigKeyExplanation {
    /// The configuration key path (e.g. `server.port`).
    pub key: String,
    /// The resolved winning value for this key, if found.
    pub value: Option<toml::Value>,
    /// The file path that provided the winning value.
    pub winning_file: Option<PathBuf>,
    /// The candidate configuration files loaded in order of precedence.
    pub candidate_precedence: Vec<PathBuf>,
}

/// Source provenance map for loaded configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSourceMap {
    /// Maps dotted configuration keys (e.g. `server.port`) to the winning configuration file.
    pub sources: BTreeMap<String, PathBuf>,
    /// The loaded configuration files in ascending precedence order (last overrides previous).
    pub loaded_files: Vec<PathBuf>,
    /// The raw merged TOML value.
    pub raw: toml::Value,
}

impl Default for ConfigSourceMap {
    fn default() -> Self {
        Self {
            sources: BTreeMap::new(),
            loaded_files: Vec::new(),
            raw: toml::Value::Table(toml::map::Map::new()),
        }
    }
}

impl ConfigSourceMap {
    /// Explain the provenance and winning value of a given configuration key.
    pub fn explain(&self, key: &str) -> ConfigKeyExplanation {
        let winning_file = self.sources.get(key).cloned();
        let value = lookup_dotted_value(&self.raw, key);
        ConfigKeyExplanation {
            key: key.to_string(),
            value,
            winning_file,
            candidate_precedence: self.loaded_files.clone(),
        }
    }
}

fn flatten_toml(
    prefix: &str,
    value: &toml::Value,
    out: &mut BTreeMap<String, PathBuf>,
    file: &Path,
) {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                let full_key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                out.insert(full_key.clone(), file.to_path_buf());
                flatten_toml(&full_key, v, out, file);
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), file.to_path_buf());
            }
        }
    }
}

fn lookup_dotted_value(root: &toml::Value, key: &str) -> Option<toml::Value> {
    let mut current = root;
    for part in key.split('.') {
        match current {
            toml::Value::Table(table) => {
                current = table.get(part)?;
            }
            _ => return None,
        }
    }
    Some(current.clone())
}

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

/// Load and validate a typed configuration for a specific environment name (e.g. "test").
pub fn load_for_env<T>(env_name: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    load_for_env_in(Path::new("."), env_name)
}

/// Load and validate a typed configuration rooted at `dir`.
pub fn load_in<T>(dir: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let figment = build_figment(dir)?;
    figment.extract::<T>().map_err(ConfigError::from)
}

/// Load and validate a typed configuration rooted at `dir` for a specific environment name.
pub fn load_for_env_in<T>(dir: &Path, env_name: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let figment = build_figment_for_env(dir, env_name)?;
    figment.extract::<T>().map_err(ConfigError::from)
}

/// Load the merged configuration as a raw TOML value.
pub fn load_raw(dir: &Path) -> Result<toml::Value> {
    let figment = build_figment(dir)?;
    figment.extract::<toml::Value>().map_err(ConfigError::from)
}

/// Load the merged configuration for a specific environment name as a raw TOML value.
pub fn load_raw_for_env(dir: &Path, env_name: &str) -> Result<toml::Value> {
    let figment = build_figment_for_env(dir, env_name)?;
    figment.extract::<toml::Value>().map_err(ConfigError::from)
}

/// Load the merged configuration as a pretty TOML string.
pub fn load_raw_pretty(dir: &Path) -> Result<String> {
    let value = load_raw(dir)?;
    toml::to_string_pretty(&value).map_err(|err| ConfigError::Validation(err.to_string()))
}

/// Load the configuration source map rooted at `dir`, tracking winning provenance per key.
pub fn source_map(dir: &Path) -> Result<ConfigSourceMap> {
    let env_name = crate::env::resolve_env_name();

    let candidates = [
        dir.join("config").join("app.toml"),
        dir.join("config").join(format!("app.{env_name}.toml")),
        dir.join("config").join("app.local.toml"),
        dir.join("config")
            .join(format!("app.{env_name}.local.toml")),
    ];

    let mut sources = BTreeMap::new();
    let mut loaded_files = Vec::new();
    let mut found = false;

    for path in candidates {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path)
                && let Ok(val) = toml::from_str::<toml::Value>(&content)
            {
                flatten_toml("", &val, &mut sources, &path);
            }
            loaded_files.push(path);
            found = true;
        }
    }

    if !found {
        return Err(ConfigError::NotFound {
            dir: dir.to_path_buf(),
        });
    }

    let raw = load_raw(dir)?;

    Ok(ConfigSourceMap {
        sources,
        loaded_files,
        raw,
    })
}

/// Load and validate a typed configuration along with its source provenance map.
pub fn load_with_provenance<T>(dir: &Path) -> Result<(T, ConfigSourceMap)>
where
    T: DeserializeOwned,
{
    let val = load_in::<T>(dir)?;
    let map = source_map(dir)?;
    Ok((val, map))
}

fn build_figment(dir: &Path) -> Result<Figment> {
    let env_name = crate::env::resolve_env_name();
    build_figment_for_env(dir, &env_name)
}

fn build_figment_for_env(dir: &Path, env_name: &str) -> Result<Figment> {
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
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();

        let result = load_raw(dir);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::NotFound { dir: found_dir } => {
                assert_eq!(found_dir, dir);
            }
            _ => panic!("expected ConfigError::NotFound"),
        }
    }

    #[test]
    fn load_raw_reads_app_toml() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        fs::create_dir_all(dir.join("config")).ok();

        write_config(dir, "app.toml", "app_name = \"test\"\nport = 3000\n");

        let result = load_raw(dir);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value.get("app_name").and_then(|v| v.as_str()), Some("test"));
        assert_eq!(value.get("port").and_then(|v| v.as_integer()), Some(3000));
    }

    #[test]
    fn load_raw_pretty_returns_valid_toml() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        fs::create_dir_all(dir.join("config")).ok();

        write_config(dir, "app.toml", "key = \"value\"\n");

        let result = load_raw_pretty(dir);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("key"));
        assert!(text.contains("value"));
    }

    #[test]
    fn load_raw_local_toml_is_loaded() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        fs::create_dir_all(dir.join("config")).ok();

        write_config(dir, "app.toml", "key = \"base\"\n");
        write_config(dir, "app.local.toml", "key = \"local\"\n");

        let result = load_raw(dir);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(value.get("key").and_then(|v| v.as_str()), Some("local"));
    }

    #[test]
    fn config_source_map_tracks_winning_file_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        fs::create_dir_all(dir.join("config")).ok();

        write_config(
            dir,
            "app.toml",
            "[server]\nport = 8080\nhost = \"127.0.0.1\"\n",
        );
        write_config(dir, "app.local.toml", "[server]\nport = 3000\n");

        let sm = source_map(dir).expect("source map");

        let port_explain = sm.explain("server.port");
        assert_eq!(port_explain.key, "server.port");
        assert_eq!(port_explain.value, Some(toml::Value::Integer(3000)));
        assert_eq!(
            port_explain.winning_file,
            Some(dir.join("config").join("app.local.toml"))
        );

        let host_explain = sm.explain("server.host");
        assert_eq!(host_explain.key, "server.host");
        assert_eq!(
            host_explain.value,
            Some(toml::Value::String("127.0.0.1".into()))
        );
        assert_eq!(
            host_explain.winning_file,
            Some(dir.join("config").join("app.toml"))
        );
    }
}
