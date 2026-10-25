use std::path::Path;
use std::process::Command;

pub fn run() {
    if !Path::new("Cargo.toml").is_file() {
        eprintln!("error: not a Rust project (Cargo.toml not found)");
        std::process::exit(1);
    }

    let has_watch = Command::new("cargo")
        .args(["watch", "--version"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if has_watch {
        let status = Command::new("cargo").args(["watch", "-x", "run"]).status();

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(err) => {
                eprintln!("error: failed to run cargo watch: {err}");
                std::process::exit(1);
            }
        }
    } else {
        println!("cargo-watch not found; running `cargo run` once.");
        println!("Install it with: cargo install cargo-watch");
        println!();

        let status = Command::new("cargo").arg("run").status();

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(err) => {
                eprintln!("error: failed to run cargo: {err}");
                std::process::exit(1);
            }
        }
    }
}
