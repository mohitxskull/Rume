use rume::prelude::*;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub app: AppConfig,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub name: String,
    #[serde(default = "default_port")]
    pub server_port: u16,
}

fn default_port() -> u16 {
    3333
}
