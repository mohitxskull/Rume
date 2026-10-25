use rume::prelude::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct Env {
    pub host: std::net::IpAddr,
    pub port: u16,
    #[serde(flatten)]
    #[allow(dead_code)]
    pub log: LogEnv,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(dead_code)]
pub struct LogEnv {
    #[serde(default)]
    pub level: Option<String>,
}
