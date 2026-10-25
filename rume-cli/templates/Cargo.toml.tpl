[package]
name = "{{name}}"
version = "0.1.0"
edition = "2024"

[dependencies]
rume = { path = "{{framework_path}}" }
serde = { version = "1.0.229", features = ["derive"] }
tokio = { version = "1.54.1", features = ["full"] }
