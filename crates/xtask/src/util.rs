//! Shared execution and workspace discovery utilities for `xtask`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Discover the Rume workspace root directory.
///
/// # Errors
///
/// Returns an error if the root `Cargo.toml` with `[workspace]` cannot be found.
pub fn workspace_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest_dir = match env::var_os("CARGO_MANIFEST_DIR") {
        Some(val) => PathBuf::from(val),
        None => env::current_dir()?,
    };

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
///
/// Why: probe `PATH` entries directly via `std::env::split_paths` instead of
/// shelling out to POSIX-only `which`, so `xtask` remains portable and avoids
/// spawning an extra subprocess.
#[must_use]
pub fn is_tool_available(name: &str) -> bool {
    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path_var).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file() || (cfg!(windows) && dir.join(format!("{name}.exe")).is_file())
    })
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
