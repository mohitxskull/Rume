//! Shared execution and workspace discovery utilities for `xtask`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Discover the Rume workspace root directory.
///
/// # Errors
///
/// Returns an error if the root `Cargo.toml` with `[workspace]` cannot be found.
pub fn workspace_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env::current_dir().unwrap().to_string_lossy().to_string()),
    );

    if manifest_dir.ends_with("crates/xtask")
        && let Some(parent) = manifest_dir.parent().and_then(|p| p.parent())
    {
        return Ok(parent.to_path_buf());
    }

    let mut curr = env::current_dir()?;
    loop {
        let manifest = curr.join("Cargo.toml");
        if manifest.exists() {
            let content = fs::read_to_string(&manifest)?;
            if content.contains("[workspace]") {
                return Ok(curr);
            }
        }
        if !curr.pop() {
            break;
        }
    }
    Err("Failed to discover Rume workspace root".into())
}

/// Check whether an external command-line tool is available in `PATH`.
#[must_use]
pub fn is_tool_available(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Execute an external command and verify successful completion.
///
/// # Errors
///
/// Returns an error if the process fails to launch or exits with a non-zero status code.
pub fn run(program: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    println!("\x1b[36m> {} {}\x1b[0m", program, args.join(" "));
    let status = Command::new(program).args(args).status()?;
    if !status.success() {
        return Err(format!(
            "Command '{program} {}' failed with status: {status}",
            args.join(" ")
        )
        .into());
    }
    Ok(())
}

/// Execute an external command with custom environment variables.
///
/// # Errors
///
/// Returns an error if the process fails to launch or exits with a non-zero status code.
pub fn run_with_env(
    program: &str,
    args: &[&str],
    env_vars: &[(&str, &str)],
) -> Result<(), Box<dyn std::error::Error>> {
    let env_disp = env_vars
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "\x1b[36m> {} {} {}\x1b[0m",
        env_disp,
        program,
        args.join(" ")
    );
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!(
            "Command '{program} {}' failed with status: {status}",
            args.join(" ")
        )
        .into());
    }
    Ok(())
}
