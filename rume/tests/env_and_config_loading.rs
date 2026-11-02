use rume::config;
use rume::env::ResolvedEnv;
use serde::Deserialize;
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    saved: Vec<(String, Option<String>)>,
}

impl EnvVarGuard {
    fn new() -> Self {
        Self { saved: Vec::new() }
    }

    fn set(&mut self, key: &str, value: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            let prev = std::env::var(key).ok();
            self.saved.push((key.to_string(), prev));
        }
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn remove(&mut self, key: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            let prev = std::env::var(key).ok();
            self.saved.push((key.to_string(), prev));
        }
        unsafe {
            std::env::remove_var(key);
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (key, prev) in self.saved.drain(..).rev() {
            unsafe {
                match prev {
                    Some(val) => std::env::set_var(key, val),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn write(path: &std::path::Path, content: &str) {
    fs::write(path, content).unwrap();
}

#[test]
fn loads_layered_env_files() {
    let _guard = ENV_LOCK.lock().unwrap();
    let mut env = EnvVarGuard::new();
    env.remove("HOST");
    env.remove("PORT");
    env.set("RUME_ENV", "development");

    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write(&root.join(".env"), "HOST=base\nPORT=1111\n");
    write(&root.join(".env.development"), "PORT=2222\n");
    write(&root.join(".env.local"), "HOST=local\n");

    let resolved = ResolvedEnv::load_in(root).unwrap();

    assert_eq!(resolved.get("HOST"), Some("local"));
    assert_eq!(resolved.get("PORT"), Some("2222"));
    assert_eq!(
        resolved.sources.get("HOST").map(String::as_str),
        Some(".env.local")
    );
    assert_eq!(
        resolved.sources.get("PORT").map(String::as_str),
        Some(".env.development")
    );
}

#[test]
fn skips_local_files_in_test_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let mut env = EnvVarGuard::new();
    env.remove("PORT");
    env.set("RUME_ENV", "test");

    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write(&root.join(".env"), "PORT=1111\n");
    write(&root.join(".env.test"), "PORT=2222\n");
    write(&root.join(".env.local"), "PORT=9999\n");

    let resolved = ResolvedEnv::load_in(root).unwrap();

    assert_eq!(resolved.get("PORT"), Some("2222"));
}

#[test]
fn resolves_file_values() {
    let _guard = ENV_LOCK.lock().unwrap();
    let mut env = EnvVarGuard::new();
    env.remove("SECRET");
    env.set("RUME_ENV", "development");

    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write(&root.join("secret.txt"), "topsecret\n");
    write(&root.join(".env"), "SECRET=file:secret.txt\n");

    let resolved = ResolvedEnv::load_in(root).unwrap();

    assert_eq!(resolved.get("SECRET"), Some("topsecret"));
}

#[derive(Deserialize)]
struct TestConfig {
    app: TestApp,
}

#[derive(Deserialize)]
struct TestApp {
    name: String,
    port: u16,
}

#[test]
fn loads_merged_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let mut env = EnvVarGuard::new();
    env.set("RUME_ENV", "development");

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("config")).unwrap();
    write(
        &root.join("config/app.toml"),
        "[app]\nname = \"Base\"\nport = 1111\n",
    );
    write(
        &root.join("config/app.development.toml"),
        "[app]\nport = 2222\n",
    );

    let value = config::load_raw(root).unwrap();
    assert_eq!(value["app"]["name"].as_str(), Some("Base"));
    assert_eq!(value["app"]["port"].as_integer(), Some(2222));

    let cfg: TestConfig = config::load_in(root).unwrap();
    assert_eq!(cfg.app.name, "Base");
    assert_eq!(cfg.app.port, 2222);
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
struct TypedEnv {
    host: std::net::IpAddr,
    port: u16,
    #[serde(default)]
    log_level: Option<String>,
}

#[test]
fn loads_typed_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let mut env = EnvVarGuard::new();
    env.remove("HOST");
    env.remove("PORT");
    env.remove("LOG_LEVEL");
    env.set("RUME_ENV", "development");

    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write(&root.join(".env"), "HOST=127.0.0.1\nPORT=3333\n");

    let env_res: TypedEnv = rume::env::load_in(root).unwrap();

    assert_eq!(env_res.host, std::net::IpAddr::from([127, 0, 0, 1]));
    assert_eq!(env_res.port, 3333);
    assert_eq!(env_res.log_level, None);
}
