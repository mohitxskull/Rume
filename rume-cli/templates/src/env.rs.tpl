use rume::serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct Env {
    pub host: std::net::IpAddr,
    pub port: u16,
    #[serde(default)]
    pub log_level: Option<String>,
}
