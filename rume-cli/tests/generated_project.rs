//! Integration test: verify that `rume new` generates a project that
//! compiles and whose tests pass, and that supported generators produce
//! compilable code.
//!
//! This is the CI quality gate for generated code: a scaffold that does not
//! compile is a defect. The test must never silently pass.

use std::process::Command;

use tempfile::TempDir;

fn rume_binary() -> String {
    // CARGO_BIN_EXE_rume is set by cargo test for integration tests and
    // points directly at the built `rume` binary.
    std::env::var("CARGO_BIN_EXE_rume").expect(
        "CARGO_BIN_EXE_rume is not set — the `rume` binary must be built before running this test",
    )
}

fn run_command(cmd: &mut Command) -> bool {
    let status = cmd.status().expect("failed to spawn command");
    status.success()
}

#[test]
fn generated_project_compiles_and_tests_pass() {
    let project_name = "rume-test-project";
    let tmp_dir = TempDir::new().expect("failed to create temp directory");
    let project_path = tmp_dir.path().join(project_name);

    // 1. Run `rume new <name>`
    let new_result = Command::new(rume_binary())
        .arg("new")
        .arg(project_name)
        .current_dir(tmp_dir.path())
        .status();
    let new_ok = match new_result {
        Ok(s) => s.success(),
        Err(err) => panic!("could not run `rume new`: {err}"),
    };
    assert!(new_ok, "`rume new {project_name}` should succeed");

    // 2. Run `cargo fmt --check` in the project dir
    let fmt_ok = run_command(
        Command::new("cargo")
            .arg("fmt")
            .arg("--check")
            .current_dir(&project_path),
    );
    assert!(
        fmt_ok,
        "`cargo fmt --check` should pass in generated project"
    );

    // 3. Run `cargo check` in the project dir
    let check_ok = run_command(
        Command::new("cargo")
            .arg("check")
            .current_dir(&project_path),
    );
    assert!(check_ok, "`cargo check` should pass in generated project");

    // 4. Run `cargo test` in the project dir
    let test_ok = run_command(Command::new("cargo").arg("test").current_dir(&project_path));
    assert!(test_ok, "`cargo test` should pass in generated project");

    // 5. Exercise generators — generated code must also compile and test clean
    for (kind, name) in [
        ("handler", "users"),
        ("resource", "posts"),
        ("middleware", "auth"),
    ] {
        let gen_ok = run_command(
            Command::new(rume_binary())
                .arg("generate")
                .arg(kind)
                .arg(name)
                .current_dir(&project_path),
        );
        assert!(
            gen_ok,
            "`rume generate {kind} {name}` should succeed in generated project"
        );
    }

    let check_ok = run_command(
        Command::new("cargo")
            .arg("check")
            .current_dir(&project_path),
    );
    assert!(
        check_ok,
        "`cargo check` should pass after running generators"
    );

    let test_ok = run_command(Command::new("cargo").arg("test").current_dir(&project_path));
    assert!(test_ok, "`cargo test` should pass after running generators");

    // TempDir is dropped automatically when it goes out of scope.
}
