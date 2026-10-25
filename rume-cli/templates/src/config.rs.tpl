use rume::prelude::*;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub app: AppConfig,
    #[allow(dead_code)]
    pub server: ServerConfig,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3333
}
