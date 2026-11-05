//! Environment loading and resolution.
//!
//! Provides [`ResolvedEnv`] and [`load`] for reading, interpolating, and
//! validating environment variables from `.env` files and the process
//! environment.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::Serialized;
use secrecy::{ExposeSecret, SecretBox};
use serde::de::DeserializeOwned;

use crate::env::error::EnvError;
use crate::env::interpolation::interpolate_value;
use crate::env::secrets::is_secret_key;

/// A resolved environment: the merged values plus where each one came from.
///
/// Secret-typed values (detected via [`is_secret_key`](crate::env::secrets::is_secret_key))
/// are stored in [`secrecy::SecretBox`] and only exposed through
/// [`expose_secret`](secrecy::SecretBox::expose_secret)
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
    pub fn load() -> Result<Self, EnvError> {
        Self::load_in(Path::new("."))
    }

    /// Load the environment rooted at `dir`.
    pub fn load_in(dir: &Path) -> Result<Self, EnvError> {
        load_merged_map_in(dir)
    }

    /// Load the environment rooted at `dir` for a specific environment name.
    pub fn load_for_env_in(dir: &Path, env_name: &str) -> Result<Self, EnvError> {
        load_merged_map_for_env_in(dir, env_name)
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

/// Load and validate a typed environment from the current directory.
pub fn load<T>() -> Result<T, EnvError>
where
    T: DeserializeOwned,
{
    load_in(Path::new("."))
}

/// Load and validate a typed environment from the current directory for a specific environment name.
pub fn load_for_env<T>(env_name: &str) -> Result<T, EnvError>
where
    T: DeserializeOwned,
{
    load_for_env_in(Path::new("."), env_name)
}

/// Load and validate a typed environment rooted at `dir`.
pub fn load_in<T>(dir: &Path) -> Result<T, EnvError>
where
    T: DeserializeOwned,
{
    let resolved = load_merged_map_in(dir)?;
    load_from_resolved(resolved)
}

/// Load and validate a typed environment rooted at `dir` for a specific environment name.
pub fn load_for_env_in<T>(dir: &Path, env_name: &str) -> Result<T, EnvError>
where
    T: DeserializeOwned,
{
    let resolved = load_merged_map_for_env_in(dir, env_name)?;
    load_from_resolved(resolved)
}

fn load_from_resolved<T>(resolved: ResolvedEnv) -> Result<T, EnvError>
where
    T: DeserializeOwned,
{
    let mut parsed: BTreeMap<String, figment::value::Value> = BTreeMap::new();
    let known_secrets: Vec<&str> = resolved
        .secrets
        .values()
        .map(|s| s.expose_secret().as_str())
        .collect();

    for (key, value) in resolved.values {
        let value = value.parse::<figment::value::Value>().map_err(|err| {
            let redacted =
                crate::env::secrets::redact_diagnostic_text(&err.to_string(), &known_secrets);
            EnvError::Validation(redacted)
        })?;
        parsed.insert(key.to_ascii_lowercase(), value.clone());
        parsed.insert(key, value);
    }
    for (key, secret) in &resolved.secrets {
        let value = secret
            .expose_secret()
            .parse::<figment::value::Value>()
            .map_err(|err| {
                let redacted =
                    crate::env::secrets::redact_diagnostic_text(&err.to_string(), &known_secrets);
                EnvError::Validation(redacted)
            })?;
        parsed.insert(key.to_ascii_lowercase(), value.clone());
        parsed.insert(key.clone(), value);
    }

    let figment = Figment::from(Serialized::defaults(parsed));
    figment
        .extract::<T>()
        .map_err(|err| EnvError::from_figment_with_secrets(&err, &known_secrets))
}

/// Load the merged environment map for the current directory.
pub fn load_merged_map() -> Result<ResolvedEnv, EnvError> {
    load_merged_map_in(Path::new("."))
}

/// Load the merged environment map rooted at `dir`.
fn load_merged_map_in(dir: &Path) -> Result<ResolvedEnv, EnvError> {
    let env_name = resolve_env_name();
    load_merged_map_for_env_in(dir, &env_name)
}

/// Load the merged environment map rooted at `dir` for a specific environment name.
fn load_merged_map_for_env_in(dir: &Path, env_name: &str) -> Result<ResolvedEnv, EnvError> {
    // Collect raw values from all file sources, lowest to highest priority.
    // Later files override earlier ones. `file:` resolution happens here, per
    // value, before interpolation.
    let mut merged: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut file_keys: BTreeSet<String> = BTreeSet::new();

    for (path, label) in dotenv_candidates(dir, env_name) {
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

            merged.insert(key.clone(), (value, label.clone()));
            file_keys.insert(key);
        }
    }

    // Process environment always wins. Overlay process env on top of the
    // merged file values so process variables participate in interpolation
    // (they can reference file-sourced variables) and take precedence over
    // file values. Process env keys are not added to `file_keys` because they
    // did not originate from an env file.
    for (key, value) in std::env::vars() {
        merged.insert(key.clone(), (value, "process env".to_string()));
    }

    // The final merged variable map (files + process env) is the interpolation
    // context: every variable can reference any other, including
    // process-overridden variables. Interpolation runs against this final map
    // rather than against file-only values, so a file variable referencing a
    // process-overridden variable resolves to the process value.
    let vars: BTreeMap<String, String> = merged
        .iter()
        .map(|(key, (value, _))| (key.clone(), value.clone()))
        .collect();

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut secrets: BTreeMap<String, SecretBox<String>> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();

    // Interpolate all values against the final merged map, then route to
    // secrets vs plain values based on the key (not the interpolated value).
    for (key, (value, source)) in &merged {
        let interpolated = interpolate_value(value, &vars)?;
        if is_secret_key(key) {
            secrets.insert(key.clone(), SecretBox::new(Box::new(interpolated)));
        } else {
            values.insert(key.clone(), interpolated);
        }
        sources.insert(key.clone(), source.clone());
    }

    Ok(ResolvedEnv {
        values,
        secrets,
        sources,
        file_keys,
    })
}

pub(crate) fn is_running_in_test() -> bool {
    if cfg!(test) {
        return true;
    }
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy();
        if path.contains("/deps/") || path.ends_with(".test") {
            return true;
        }
    }
    false
}

/// Resolve the active environment name.
///
/// Precedence: `RUME_ENV`, then `RAILS_ENV`, then `NODE_ENV`, then
/// `"test"` if running in test harness, otherwise `"development"`.
pub fn resolve_env_name() -> String {
    for var in ["RUME_ENV", "RAILS_ENV", "NODE_ENV"] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
        {
            return value;
        }
    }
    if is_running_in_test() {
        return "test".to_string();
    }
    "development".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- Sprint 2: environment resolution precedence tests ---

    #[test]
    fn interpolation_uses_process_overridden_value() {
        // Simulates the final merged map where process env has overridden a
        // file variable. A reference to that key resolves to the process
        // value, not the file value.
        let mut vars = BTreeMap::new();
        vars.insert("HOST".to_string(), "file-host".to_string());
        // Process env overrides HOST:
        vars.insert("HOST".to_string(), "process-host".to_string());

        assert_eq!(
            interpolate_value("https://${HOST}/api", &vars).unwrap(),
            "https://process-host/api"
        );
    }

    #[test]
    fn interpolation_process_refs_file_variable() {
        // A value can reference another variable in the same merged map,
        // simulating a process env variable referencing a file variable.
        let mut vars = BTreeMap::new();
        vars.insert("FILE_HOST".to_string(), "file-host".to_string());
        vars.insert(
            "PROCESS_URL".to_string(),
            "http://${FILE_HOST}:8080".to_string(),
        );

        assert_eq!(
            interpolate_value("${PROCESS_URL}", &vars).unwrap(),
            "http://file-host:8080"
        );
    }

    #[test]
    fn interpolation_file_refs_process_overridden_variable() {
        // A file variable referencing a process-overridden variable resolves
        // to the process value since both are in the same interpolation map.
        let mut vars = BTreeMap::new();
        vars.insert("BASE_HOST".to_string(), "file-host".to_string());
        // Process env override:
        vars.insert("BASE_HOST".to_string(), "process-host".to_string());
        vars.insert("URL".to_string(), "https://${BASE_HOST}/api".to_string());

        assert_eq!(
            interpolate_value("${URL}", &vars).unwrap(),
            "https://process-host/api"
        );
    }

    #[test]
    fn interpolation_file_refs_another_file_variable() {
        // Two file-sourced variables in the same .env file can reference each
        // other.
        let mut vars = BTreeMap::new();
        vars.insert("HOST".to_string(), "localhost".to_string());
        vars.insert("URL".to_string(), "http://${HOST}:3000".to_string());

        assert_eq!(
            interpolate_value("${URL}", &vars).unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn load_merged_map_routes_secret_to_secrets_map() {
        // A secret key written to a .env file must be routed to the secrets
        // map (SecretBox), not the plain values map, avoiding plaintext
        // exposure.
        let dir = std::env::temp_dir().join("rume_secret_route_test");
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
}
