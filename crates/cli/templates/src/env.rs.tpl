use rume::prelude::*;

// Why: `rume::app::init_tracing()` configures log filtering directly from `RUST_LOG`,
// so `Env` omits the unused `LogEnv` / `log` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct Env {
    pub host: std::net::IpAddr,
    pub port: u16,
}
