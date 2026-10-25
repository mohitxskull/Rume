use rume::config;

#[derive(rume::serde::Deserialize)]
struct Config {
    app_name: String,
    #[serde(default = "default_port")]
    server_port: u16,
}

fn default_port() -> u16 {
    3333
}

#[test]
fn config_loads() {
    let config: Config = rume::config::load().expect("config should load");
    assert!(!config.app_name.is_empty());
}
