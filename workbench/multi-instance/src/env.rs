use serde::Deserialize;

// Why: `rume::app::init_tracing()` configures log filtering directly from `RUST_LOG`,
// so `Env` omits the unused `LogEnv` / `log` field.
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
