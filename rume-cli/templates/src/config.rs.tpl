use rume::serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub app: AppConfig,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub name: String,
}

pub fn load() -> Result<Config, rume::config::ConfigError> {
    rume::config::load::<Config>()
}
