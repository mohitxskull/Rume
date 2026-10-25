use std::path::Path;
use std::process::Command;

pub fn run() {
    if !Path::new("Cargo.toml").is_file() {
        eprintln!("error: not a Rust project (Cargo.toml not found)");
        std::process::exit(1);
    }

    let status = Command::new("cargo").arg("test").status();

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("error: failed to run cargo test: {err}");
            std::process::exit(1);
        }
    }
}
