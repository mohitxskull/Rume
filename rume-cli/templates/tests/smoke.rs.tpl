use rume::config;

#[derive(rume::serde::Deserialize)]
struct Config {
    app: AppConfig,
}

#[derive(rume::serde::Deserialize)]
struct AppConfig {
    name: String,
}

#[test]
fn config_loads() {
    let config: Config = config::load().expect("config should load");
    assert!(!config.app.name.is_empty());
}
