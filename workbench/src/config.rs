use rume::persistence::DatabaseConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub app: AppConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub name: String,
    #[serde(default = "default_api_prefix")]
    pub api_prefix: String,
}

fn default_api_prefix() -> String {
    "/api/v1".to_string()
}
