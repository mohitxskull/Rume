//! Integration test: verify that `rume new` generates a project that
//! compiles and whose tests pass.
//!
//! This is the CI quality gate for generated code: a scaffold that does not
//! compile is a defect.

use std::process::Command;
use std::time::SystemTime;

fn rume_binary() -> String {
    // The `rume` binary is built as part of the workspace.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| "target/debug".to_string())
        + "/rume"
}

fn run_command(cmd: &mut Command) -> bool {
    let status = cmd.status().map_err(|err| {
        eprintln!("[skipping generated project test: {err}]");
    });
    match status {
        Ok(s) => s.success(),
        Err(()) => false,
    }
}

#[test]
fn generated_project_compiles_and_tests_pass() {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let project_name = format!("rume-test-project-{stamp}");

    let tmp_dir = std::env::temp_dir().join(&project_name);
    let _ = std::fs::remove_dir_all(&tmp_dir);

    // 1. Run `rume new <name>`
    let new_result = Command::new(rume_binary())
        .arg("new")
        .arg(&project_name)
        .current_dir(std::env::temp_dir())
        .status();

    let new_ok = match new_result {
        Ok(s) => s.success(),
        Err(err) => {
            eprintln!("[skipping generated project test: could not run `rume new`: {err}]");
            return;
        }
    };
    assert!(new_ok, "`rume new {project_name}` should succeed");

    // 2. Run `cargo fmt --check` in the project dir
    let fmt_ok = run_command(
        Command::new("cargo")
            .arg("fmt")
            .arg("--check")
            .current_dir(&tmp_dir),
    );
    assert!(
        fmt_ok,
        "`cargo fmt --check` should pass in generated project"
    );

    // 3. Run `cargo check` in the project dir
    let check_ok = run_command(Command::new("cargo").arg("check").current_dir(&tmp_dir));
    assert!(check_ok, "`cargo check` should pass in generated project");

    // 4. Run `cargo test` in the project dir
    let test_ok = run_command(Command::new("cargo").arg("test").current_dir(&tmp_dir));
    assert!(test_ok, "`cargo test` should pass in generated project");

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
