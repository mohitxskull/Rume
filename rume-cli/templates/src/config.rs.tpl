use rume::serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub app_name: String,
    #[serde(default = "default_port")]
    pub server_port: u16,
}

fn default_port() -> u16 {
    3333
}
