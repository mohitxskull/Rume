use rume::prelude::*;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub app: AppConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub name: String,
}
