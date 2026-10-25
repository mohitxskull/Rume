use std::fs;
use std::path::Path;

// Resolved at compile time so generated projects build against this checkout
// during pre-v1 development. Switch to a crates.io version before publishing.
const FRAMEWORK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rume");

pub fn run(name: &str) {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        eprintln!("error: project name must contain only letters, numbers, '-' or '_'");
        std::process::exit(1);
    }

    let root = Path::new(name);
    if root.exists() {
        eprintln!("error: directory '{name}' already exists");
        std::process::exit(1);
    }

    if let Err(err) = scaffold(root, name) {
        eprintln!("error: failed to scaffold project: {err}");
        let _ = fs::remove_dir_all(root);
        std::process::exit(1);
    }

    println!("Created project `{name}`");
    println!();
    println!("Next steps:");
    println!("  cd {name}");
    println!("  cargo run");
    println!();
    println!("Then open http://127.0.0.1:3333/health");
}

fn scaffold(root: &Path, name: &str) -> Result<(), String> {
    fs::create_dir_all(root.join("src")).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("config")).map_err(|e| e.to_string())?;

    write(
        &root.join("Cargo.toml"),
        &format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
rume = {{ path = "{FRAMEWORK_PATH}" }}
serde = {{ version = "1.0.229", features = ["derive"] }}
tokio = {{ version = "1.54.1", features = ["full"] }}
"#
        ),
    )?;

    write(
        &root.join("src/main.rs"),
        r#"use rume::axum::{Json, Router, routing::get};
use rume::serde::Serialize;

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/health", get(|| async { Json(Health { status: "ok" }) }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3333")
        .await
        .expect("failed to bind address");

    println!("listening on http://127.0.0.1:3333");
    rume::axum::serve(listener, app).await.expect("server error");
}
"#,
    )?;

    write(&root.join(".env"), "HOST=127.0.0.1\nPORT=3333\n")?;

    write(&root.join(".env.example"), "HOST=127.0.0.1\nPORT=3333\n")?;

    write(
        &root.join("config/app.toml"),
        &format!("[app]\nname = \"{name}\"\n"),
    )?;

    write(
        &root.join("README.md"),
        &format!(
            r#"# {name}

A Rume application.

## Run

```sh
cargo run
```

Then visit http://127.0.0.1:3333/health.

## Environment

Copy `.env.example` to `.env` and adjust as needed.
"#
        ),
    )?;

    Ok(())
}

fn write(path: &Path, content: &str) -> Result<(), String> {
    fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}
