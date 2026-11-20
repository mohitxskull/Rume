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

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Check whether an external command-line tool is available in `PATH`.
///
/// Why: probe `PATH` entries directly via `std::env::split_paths` instead of
/// shelling out to POSIX-only `which`, and verify executable permissions on Unix
/// (or `PATHEXT` extensions on Windows) so a non-executable regular file does not
/// falsely report a tool as installed.
#[must_use]
pub fn is_tool_available(name: &str) -> bool {
    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };
    is_tool_available_in_path(name, &path_var)
}

fn is_tool_available_in_path(name: &str, path_var: &std::ffi::OsStr) -> bool {
    env::split_paths(path_var).any(|dir| {
        #[cfg(windows)]
        {
            if is_executable_file(&dir.join(name)) {
                return true;
            }
            let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                .any(|ext| is_executable_file(&dir.join(format!("{name}{ext}"))))
        }
        #[cfg(not(windows))]
        {
            is_executable_file(&dir.join(name))
        }
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

#[cfg(all(test, unix))]
mod tests {
    use super::is_tool_available_in_path;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn is_tool_available_requires_execute_bit_on_unix() {
        let dir = std::env::temp_dir().join(format!("rume_xtask_path_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let non_exec = dir.join("fake-nonexec-tool");
        fs::write(&non_exec, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&non_exec, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_tool_available_in_path(
            "fake-nonexec-tool",
            OsStr::new(&dir)
        ));

        let exec = dir.join("fake-exec-tool");
        fs::write(&exec, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&exec, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_tool_available_in_path(
            "fake-exec-tool",
            OsStr::new(&dir)
        ));

        let _ = fs::remove_dir_all(&dir);
    }
}
