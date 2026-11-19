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
use crate::env::secrets::{is_secret_key, value_looks_sensitive};

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
    let secret_keys: BTreeSet<String> = resolved.secrets.keys().cloned().collect();

    // Why: insert all exact keys before any lowercase aliases so an uppercase
    // key (e.g. `DATABASE_URL` in `secrets`) can never overwrite an explicit
    // lowercase key (e.g. `database_url` in `values`).
    for (key, value) in &resolved.values {
        parsed.insert(key.clone(), figment::value::Value::from(value.as_str()));
    }
    for (key, secret) in &resolved.secrets {
        parsed.insert(
            key.clone(),
            figment::value::Value::from(secret.expose_secret().as_str()),
        );
    }
    for (key, value) in &resolved.values {
        parsed
            .entry(key.to_ascii_lowercase())
            .or_insert_with(|| figment::value::Value::from(value.as_str()));
    }
    for (key, secret) in &resolved.secrets {
        parsed
            .entry(key.to_ascii_lowercase())
            .or_insert_with(|| figment::value::Value::from(secret.expose_secret().as_str()));
    }

    // Why: `extract_lossy` uses figment's `LossyInterpreter`, which coerces a
    // raw `Value::String` into `bool`/`u16`/`usize`/etc. at extraction time
    // ("true" -> true, "8080" -> 8080) while keeping `String`/`SecretBox<String>`
    // targets as the literal string ("001234" keeps its leading zeros).
    //
    // `LossyInterpreter` does not parse structured strings into sequences/maps,
    // so if extraction fails because a target requested a sequence/map/struct/tuple
    // for a `Value::String` key, parse just that key as a structured `Value`
    // (`Array`/`Dict`) and retry. Keeping `Value::String` as the default
    // ensures `String` fields still receive literal text (including "[a,b]").
    let max_upgrades = parsed.len();
    for _ in 0..=max_upgrades {
        let figment = Figment::from(Serialized::defaults(&parsed));
        match figment.extract_lossy::<T>() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !upgrade_structured_fields(&err, &mut parsed) {
                    return Err(EnvError::from_figment_with_secrets(
                        err,
                        &known_secrets,
                        &secret_keys,
                    ));
                }
            }
        }
    }

    let figment = Figment::from(Serialized::defaults(&parsed));
    figment
        .extract_lossy::<T>()
        .map_err(|err| EnvError::from_figment_with_secrets(err, &known_secrets, &secret_keys))
}

fn expects_structured_target(expected: &str) -> bool {
    expected == "a sequence"
        || expected == "a map"
        || expected.starts_with("a tuple")
        || expected.starts_with("struct ")
}

fn upgrade_structured_fields(
    err: &figment::Error,
    parsed: &mut BTreeMap<String, figment::value::Value>,
) -> bool {
    use figment::error::{Actual, Kind};
    use figment::value::Value;

    let mut upgraded = false;
    for sub_err in err.clone() {
        let Kind::InvalidType(Actual::Str(_), ref expected) = sub_err.kind else {
            continue;
        };
        if !expects_structured_target(expected) || sub_err.path.len() != 1 {
            continue;
        }
        let key = &sub_err.path[0];
        if let Some(Value::String(_, raw)) = parsed.get(key)
            && let Ok(structured @ (Value::Array(..) | Value::Dict(..))) = raw.parse::<Value>()
        {
            parsed.insert(key.clone(), structured);
            upgraded = true;
        }
    }
    upgraded
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
    validate_env_name(env_name)?;

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

    // Why: only interpolate values for keys that originated from an env file
    // (`file_keys`). Inherited OS variables (e.g. `PS1`, shell prompts, or
    // CI runner vars) frequently contain literal `${...}` syntax unrelated to
    // Rume; interpolating them would fail boot on healthy environments.
    // Process-env values still remain in `vars` as lookup targets so file
    // variables can reference process-overridden variables.
    for (key, (value, source)) in &merged {
        let resolved_value = if file_keys.contains(key) {
            interpolate_value(value, &vars)?
        } else {
            value.clone()
        };
        if is_secret_key(key) || value_looks_sensitive(&resolved_value) {
            secrets.insert(key.clone(), SecretBox::new(Box::new(resolved_value)));
        } else {
            values.insert(key.clone(), resolved_value);
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

/// Returns `true` if `env_name` is a safe environment identifier (`^[A-Za-z0-9_-]+$`).
///
/// Why: `env_name` is interpolated into `.env.{env_name}` and `config/app.{env_name}.toml`
/// filenames; disallowing path separators, dots, and whitespace prevents path traversal.
pub(crate) fn is_valid_env_name(env_name: &str) -> bool {
    !env_name.is_empty()
        && env_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) fn validate_env_name(env_name: &str) -> Result<(), EnvError> {
    if is_valid_env_name(env_name) {
        Ok(())
    } else {
        Err(EnvError::Validation(format!(
            "invalid environment name '{env_name}': must match ^[A-Za-z0-9_-]+$"
        )))
    }
}

/// Resolve the active environment name.
///
/// Precedence: `RUME_ENV`, then `RAILS_ENV`, then `NODE_ENV`, then
/// `"test"` if running in test harness, otherwise `"development"`.
///
/// Values that fail `^[A-Za-z0-9_-]+$` validation are ignored to prevent path
/// traversal when callers construct `.env.<name>` or `app.<name>.toml` paths.
pub fn resolve_env_name() -> String {
    for var in ["RUME_ENV", "RAILS_ENV", "NODE_ENV"] {
        if let Ok(value) = std::env::var(var)
            && is_valid_env_name(&value)
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
        // Why: fail closed on `fs::canonicalize` errors instead of falling back to raw paths,
        // and read `&canonical_full` rather than `&full` so dangling targets or symlink escapes
        // cannot bypass directory containment.
        let canonical_dir = fs::canonicalize(dir)?;
        let canonical_full = fs::canonicalize(&full)?;
        if !canonical_full.starts_with(&canonical_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("file: path escapes directory: {path}"),
            ));
        }

        let contents = fs::read_to_string(&canonical_full)?;
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

    #[test]
    fn file_value_nonexistent_and_symlink_escape_fail_closed() {
        let dir = tempfile::tempdir().unwrap();

        let missing_err = resolve_value("file:does_not_exist.key".to_string(), dir.path())
            .expect_err("missing file: target must fail closed");
        assert_eq!(missing_err.kind(), std::io::ErrorKind::NotFound);

        let missing_dir = dir.path().join("nonexistent_subdir");
        let missing_dir_err = resolve_value("file:secret.key".to_string(), &missing_dir)
            .expect_err("nonexistent base dir must fail closed");
        assert_eq!(missing_dir_err.kind(), std::io::ErrorKind::NotFound);

        #[cfg(unix)]
        {
            let outside_dir = tempfile::tempdir().unwrap();
            let outside_secret = outside_dir.path().join("outside.key");
            std::fs::write(&outside_secret, "top-secret").unwrap();

            let escape_link = dir.path().join("escape.key");
            std::os::unix::fs::symlink(&outside_secret, &escape_link).unwrap();

            let escape_err = resolve_value("file:escape.key".to_string(), dir.path())
                .expect_err("symlink pointing outside dir must be rejected");
            assert_eq!(escape_err.kind(), std::io::ErrorKind::PermissionDenied);
        }
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

    #[test]
    fn raw_values_are_preserved_while_target_types_coerce() {
        // RC-03: raw env strings must reach typed extraction as literal
        // strings so `String`/`SecretString` fields succeed, while figment
        // still coerces to `u16`/`bool` when the target type requests it.
        //
        // Why: `String::parse::<Value>()` uses `FromStr`, which coerces
        // "001234" -> 123 (drops leading zeros), "true" -> Bool, "8080" ->
        // Num. A coerced Num/Bool cannot be deserialized into a String field,
        // so typed extraction would fail. Storing `Value::String` (raw) and
        // letting figment coerce on demand keeps "001234" intact for String
        // fields and still yields u16 8080 / bool true when requested.

        let mut resolved = ResolvedEnv::default();
        resolved
            .values
            .insert("PIN".to_string(), "001234".to_string());
        resolved
            .values
            .insert("PORT".to_string(), "8080".to_string());
        resolved
            .values
            .insert("FLAG".to_string(), "true".to_string());

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        struct Typed {
            pin: String,
            port: u16,
            flag: bool,
        }

        let typed: Typed = load_from_resolved(resolved).unwrap();
        assert_eq!(typed.pin, "001234");
        assert_eq!(typed.port, 8080);
        assert!(typed.flag);
    }

    #[test]
    fn typed_collections_load_while_strings_stay_literal() {
        let mut resolved = ResolvedEnv::default();
        resolved
            .values
            .insert("PORTS".to_string(), "[8080,8081]".to_string());
        resolved.values.insert(
            "LABELS".to_string(),
            "{env=\"prod\",tier=\"web\"}".to_string(),
        );
        resolved
            .values
            .insert("RAW_LIST".to_string(), "[8080,8081]".to_string());
        resolved
            .values
            .insert("PIN".to_string(), "001234".to_string());

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        struct Typed {
            ports: Vec<u16>,
            labels: BTreeMap<String, String>,
            raw_list: String,
            pin: String,
        }

        let typed: Typed = load_from_resolved(resolved).unwrap();
        assert_eq!(typed.ports, vec![8080, 8081]);
        assert_eq!(typed.labels.get("env").map(String::as_str), Some("prod"));
        assert_eq!(typed.labels.get("tier").map(String::as_str), Some("web"));
        assert_eq!(typed.raw_list, "[8080,8081]");
        assert_eq!(typed.pin, "001234");
    }

    #[test]
    fn secret_type_error_redacts_quoted_list_without_leaking() {
        let mut resolved = ResolvedEnv::default();
        resolved.secrets.insert(
            "API_SECRET".to_string(),
            SecretBox::new(Box::new(
                "[\"top-secret-alpha\",\"top-secret-beta\"]".to_string(),
            )),
        );

        #[derive(Debug, serde::Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        struct BadSecret {
            #[allow(dead_code)]
            api_secret: u32,
        }

        let err = load_from_resolved::<BadSecret>(resolved).unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("top-secret-alpha"),
            "error leaked first secret element"
        );
        assert!(
            !msg.contains("top-secret-beta"),
            "error leaked second secret element"
        );
        assert!(
            msg.contains("[redacted]"),
            "error should contain [redacted]"
        );

        // Non-secret diagnostic text must be preserved.
        let mut non_secret_resolved = ResolvedEnv::default();
        non_secret_resolved
            .values
            .insert("PORT".to_string(), "bad-port".to_string());

        #[derive(Debug, serde::Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        struct BadPort {
            #[allow(dead_code)]
            port: u16,
        }

        let err = load_from_resolved::<BadPort>(non_secret_resolved).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bad-port"),
            "non-secret diagnostic value should be preserved"
        );
    }

    #[test]
    fn secret_alias_does_not_overwrite_exact_lowercase_key() {
        let mut resolved = ResolvedEnv::default();
        resolved.values.insert(
            "database_url".to_string(),
            "postgres://localhost/dev_db".to_string(),
        );
        resolved.secrets.insert(
            "DATABASE_URL".to_string(),
            SecretBox::new(Box::new("postgres://user:pass@prod/db".to_string())),
        );
        resolved.secrets.insert(
            "API_TOKEN".to_string(),
            SecretBox::new(Box::new("tok-123".to_string())),
        );

        #[derive(serde::Deserialize)]
        struct Typed {
            database_url: String,
            api_token: SecretBox<String>,
        }

        let typed: Typed = load_from_resolved(resolved).unwrap();
        assert_eq!(typed.database_url, "postgres://localhost/dev_db");
        assert_eq!(typed.api_token.expose_secret(), "tok-123");
    }

    #[test]
    fn invalid_env_name_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        for bad in ["../evil", "a/b", "a\\b", "", "prod.local", "dev space"] {
            let err = ResolvedEnv::load_for_env_in(temp.path(), bad).unwrap_err();
            assert!(
                matches!(err, EnvError::Validation(_)),
                "expected Validation error for env_name={bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn valid_env_names_are_accepted() {
        for good in ["development", "test", "prod-1", "staging_eu_2"] {
            assert!(is_valid_env_name(good));
        }
    }
}
