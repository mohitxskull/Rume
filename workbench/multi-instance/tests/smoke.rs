use workbench::config::Config;

#[test]
fn config_loads() {
    let config: Config = rume::config::load_for_env("test").expect("config should load");
    assert_eq!(config.app.name, "workbench");
    assert_eq!(config.database.max_connections, 10);
}
