use rume::config;
use rume::prelude::*;

#[derive(Deserialize)]
struct Config {
    app: AppConfig,
    server: ServerConfig,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct AppConfig {
    name: String,
    #[serde(default = "default_port")]
    server_port: u16,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ServerConfig {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3333
}

#[test]
fn config_loads() {
    // Ensure test-specific config is used.
    unsafe { std::env::set_var("RUME_ENV", "test"); }
    let config: Config = rume::config::load().expect("config should load");
    assert!(!config.app.name.is_empty());
}
