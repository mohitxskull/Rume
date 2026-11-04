use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct Env {
    #[serde(default = "default_host")]
    pub host: std::net::IpAddr,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_database_url")]
    pub database_url: String,
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(flatten)]
    #[allow(dead_code)]
    pub log: LogEnv,
}

fn default_host() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
}

fn default_port() -> u16 {
    8080
}

fn default_database_url() -> String {
    "postgres://postgres:postgres@localhost:5432/workbench".to_string()
}

fn default_redis_url() -> String {
    "redis://localhost:6379".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(dead_code)]
pub struct LogEnv {
    #[serde(default)]
    pub level: Option<String>,
}
