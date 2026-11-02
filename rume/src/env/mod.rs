//! Environment loading for Rume applications.
//!
//! Rume does not use a runtime schema DSL for environment variables. Instead,
//! a project declares its environment as an ordinary Rust struct deriving
//! `serde::Deserialize`, and [`load`] validates it at boot using `figment`.
//!
//! ```ignore
//! #[derive(Debug, serde::Deserialize)]
//! #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
//! struct Env {
//!     pub host: std::net::IpAddr,
//!     pub port: u16,
//!     pub app_key: secrecy::SecretBox<String>,
//! }
//!
//! let env: Env = rume::env::load()?;
//! ```

pub mod error;
pub mod interpolation;
pub mod loading;
pub mod secrets;

// Re-export the public env surface at the module root for convenience.
pub use error::EnvError;
pub use interpolation::interpolate_value;
pub use loading::{ResolvedEnv, load, load_in, load_merged_map, resolve_env_name};
pub use secrets::{is_secret_key, redact_value, value_looks_sensitive};

pub type Result<T> = std::result::Result<T, EnvError>;
