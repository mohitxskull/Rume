//! CLI workflow integration tests for `rume generate`, `rume codegen`, and `rume db`.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn rume_binary() -> String {
    std::env::var("CARGO_BIN_EXE_rume").expect(
        "CARGO_BIN_EXE_rume is not set — the `rume` binary must be built before running this test",
    )
}

fn framework_path() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/core")
}

fn run_command(cmd: &mut Command) -> (bool, String, String) {
    let output = cmd.output().expect("failed to execute command");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn test_cli_generate_resource_middleware_and_codegen_compiles() {
    let tmp = TempDir::new().expect("create temp dir");
    let project_name = "workflow-app";
    let project_dir = tmp.path().join(project_name);

    // 1. Scaffold project
    let (new_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .arg("new")
            .arg(project_name)
            .arg("--path")
            .arg(framework_path())
            .current_dir(tmp.path()),
    );
    assert!(
        new_ok,
        "rume new failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // 2. Generate resource
    let (res_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .args(["generate", "resource", "products"])
            .current_dir(&project_dir),
    );
    assert!(
        res_ok,
        "rume generate resource failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Verify generated resource file exists
    let resource_file = project_dir.join("src").join("handlers").join("products.rs");
    assert!(
        resource_file.is_file(),
        "resource file should be generated at src/handlers/products.rs"
    );

    // 3. Generate middleware
    let (mid_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .args(["generate", "middleware", "rate_limit"])
            .current_dir(&project_dir),
    );
    assert!(
        mid_ok,
        "rume generate middleware failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    let middleware_file = project_dir
        .join("src")
        .join("middleware")
        .join("rate_limit.rs");
    assert!(
        middleware_file.is_file(),
        "middleware file should be generated at src/middleware/rate_limit.rs"
    );

    // 4. Run `rume codegen --dry-run`
    let (code_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .args(["codegen", "--dry-run"])
            .current_dir(&project_dir),
    );
    assert!(
        code_ok,
        "rume codegen --dry-run failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // 5. Run `cargo check` to verify the generated project compiles cleanly
    let (check_ok, stdout, stderr) =
        run_command(Command::new("cargo").arg("check").current_dir(&project_dir));
    assert!(
        check_ok,
        "cargo check failed in generated project:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn test_cli_db_migration_generation_and_compilation() {
    let tmp = TempDir::new().expect("create temp dir");
    let project_name = "migration-app";
    let project_dir = tmp.path().join(project_name);

    // 1. Scaffold project
    let (new_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .arg("new")
            .arg(project_name)
            .arg("--path")
            .arg(framework_path())
            .current_dir(tmp.path()),
    );
    assert!(
        new_ok,
        "rume new failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    // 2. Generate migration
    let (mig_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .args(["make:migration", "create_orders_table"])
            .current_dir(&project_dir),
    );
    assert!(
        mig_ok,
        "rume make:migration failed:\nstdout: {stdout}\nstderr: {stderr}"
    );

    let migrations_dir = project_dir.join("migrations");
    assert!(migrations_dir.is_dir());

    let entries: Vec<_> = fs::read_dir(&migrations_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let has_orders_up = entries
        .iter()
        .any(|n| n.contains("create_orders_table") && n.ends_with(".up.sql"));
    let has_orders_down = entries
        .iter()
        .any(|n| n.contains("create_orders_table") && n.ends_with(".down.sql"));
    assert!(has_orders_up, "must have create_orders_table.up.sql");
    assert!(has_orders_down, "must have create_orders_table.down.sql");

    // 3. Inspect db status offline
    let (status_ok, stdout, stderr) = run_command(
        Command::new(rume_binary())
            .args(["db", "status"])
            .current_dir(&project_dir),
    );
    assert!(
        status_ok,
        "rume db status failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("create_orders_table"));

    // 4. Verify that embed_migrations! compiles the new migration cleanly
    let (check_ok, stdout, stderr) =
        run_command(Command::new("cargo").arg("check").current_dir(&project_dir));
    assert!(
        check_ok,
        "cargo check failed after migration creation:\nstdout: {stdout}\nstderr: {stderr}"
    );
}
