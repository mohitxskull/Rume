use serde::Deserialize;
use std::fs;
use tempfile::tempdir;

#[derive(Debug, Deserialize)]
struct TypedEnvSecret {
    #[allow(dead_code)]
    api_key: u32,
}

#[derive(Debug, Deserialize)]
struct TypedEnvDbUrl {
    #[allow(dead_code)]
    database_url: u32,
}

#[derive(Debug, Deserialize)]
struct TypedConfigSecret {
    #[allow(dead_code)]
    secret_token: u32,
}

#[derive(Debug, Deserialize)]
struct TypedConfigDbUrl {
    #[allow(dead_code)]
    db_url: u32,
}

#[derive(Debug, Deserialize)]
struct TypedEnvPort {
    #[allow(dead_code)]
    port: u16,
}

#[test]
fn typed_env_with_invalid_secret_field_does_not_disclose_secret() {
    let dir = tempdir().unwrap();
    let secret = "top_secret_api_key_abc_123";
    fs::write(dir.path().join(".env"), format!("API_KEY={secret}\n")).unwrap();

    let err = rume::env::load_in::<TypedEnvSecret>(dir.path()).unwrap_err();
    let msg = err.to_string();

    assert!(
        !msg.contains(secret),
        "secret was exposed in env validation error: {msg}"
    );
    assert!(
        msg.contains("[redacted]"),
        "error message should indicate redaction: {msg}"
    );
}

#[test]
fn typed_env_with_invalid_credential_url_does_not_disclose_password() {
    let dir = tempdir().unwrap();
    let password = "super_secret_db_pass_999";
    let url = format!("postgres://admin:{password}@localhost:5432/app");
    fs::write(dir.path().join(".env"), format!("DATABASE_URL={url}\n")).unwrap();

    let err = rume::env::load_in::<TypedEnvDbUrl>(dir.path()).unwrap_err();
    let msg = err.to_string();

    assert!(
        !msg.contains(password),
        "database password was exposed in env error: {msg}"
    );
    assert!(
        msg.contains("[redacted]"),
        "error message should indicate redacted URL password: {msg}"
    );
}

#[test]
fn typed_config_with_invalid_secret_field_does_not_disclose_secret() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let secret = "my_confidential_token_xyz_789";
    fs::write(
        config_dir.join("app.toml"),
        format!("secret_token = \"{secret}\"\n"),
    )
    .unwrap();

    let err = rume::config::load_in::<TypedConfigSecret>(dir.path()).unwrap_err();
    let msg = err.to_string();

    assert!(
        !msg.contains(secret),
        "secret token was exposed in config validation error: {msg}"
    );
    assert!(
        msg.contains("[redacted]"),
        "error message should indicate redaction: {msg}"
    );
}

#[test]
fn typed_config_with_credential_url_does_not_disclose_password() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();

    let password = "secret_mysql_pass_456";
    let url = format!("mysql://root:{password}@127.0.0.1:3306/db");
    fs::write(config_dir.join("app.toml"), format!("db_url = \"{url}\"\n")).unwrap();

    let err = rume::config::load_in::<TypedConfigDbUrl>(dir.path()).unwrap_err();
    let msg = err.to_string();

    assert!(
        !msg.contains(password),
        "mysql password was exposed in config error: {msg}"
    );
    assert!(
        msg.contains("[redacted]"),
        "error message should indicate redacted password: {msg}"
    );
}

#[test]
fn non_secret_error_retains_diagnostic_clarity() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".env"), "PORT=not_a_valid_number\n").unwrap();

    let err = rume::env::load_in::<TypedEnvPort>(dir.path()).unwrap_err();
    let msg = err.to_string();

    assert!(
        msg.contains("not_a_valid_number"),
        "non-secret error should retain input diagnostic: {msg}"
    );
    assert!(
        !msg.contains("[redacted]"),
        "non-secret error should not be redacted: {msg}"
    );
}
