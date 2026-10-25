use rume::config;
use rume::prelude::*;

#[derive(Deserialize)]
struct Config {
    app: AppConfig,
}

#[derive(Deserialize)]
struct AppConfig {
    name: String,
    #[serde(default = "default_port")]
    server_port: u16,
}

fn default_port() -> u16 {
    3333
}

#[test]
fn config_loads() {
    let config: Config = config::load().expect("config should load");
    assert!(!config.app.name.is_empty());
}
