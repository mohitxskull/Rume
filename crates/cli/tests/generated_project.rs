//! Integration test: verify that `rume new` generates a portable project,
//! local development path overrides work, all generators produce compilable code,
//! conflict detection prevents overwriting, and CLI inspection commands return
//! valid machine-readable JSON.
//!
//! This is the CI quality gate for Milestone 2: a scaffold or generator that
//! does not compile or produce valid outputs is a defect.

mod common;

use crate::common::framework_path;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn rume_binary() -> String {
    std::env::var("CARGO_BIN_EXE_rume").expect(
        "CARGO_BIN_EXE_rume is not set — the `rume` binary must be built before running this test",
    )
}

fn run_command(cmd: &mut Command) -> bool {
    let status = cmd.status().expect("failed to spawn command");
    status.success()
}

#[test]
fn new_project_defaults_to_portable_dependency() {
    let tmp_dir = TempDir::new().expect("failed to create temp directory");
    let project_name = "portable-project";

    // Run `rume new <name>` without path override
    let status = Command::new(rume_binary())
        .arg("new")
        .arg(project_name)
        .current_dir(tmp_dir.path())
        .status()
        .expect("failed to run `rume new`");
    assert!(status.success(), "`rume new` should succeed");

    let cargo_toml_path = tmp_dir.path().join(project_name).join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path).expect("read generated Cargo.toml");

    // Must depend on published version without local path
    assert!(
        content.contains("rume = \"0.1.0\""),
        "Generated Cargo.toml should contain portable version dependency, got:\n{content}"
    );
    assert!(
        !content.contains("path ="),
        "Generated Cargo.toml should not contain path dependency in portable mode"
    );
}

#[test]
fn generated_project_compiles_and_tests_pass_with_all_generators() {
    let project_name = "rume-test-project";
    let tmp_dir = TempDir::new().expect("failed to create temp directory");
    let project_path = tmp_dir.path().join(project_name);

    // 1. Run `rume new <name> --path <framework_path>`
    let status = Command::new(rume_binary())
        .arg("new")
        .arg(project_name)
        .arg("--path")
        .arg(framework_path())
        .current_dir(tmp_dir.path())
        .status()
        .expect("could not run `rume new`");
    assert!(status.success(), "`rume new {project_name}` should succeed");

    // Verify local development path override was injected into Cargo.toml
    let cargo_toml =
        fs::read_to_string(project_path.join("Cargo.toml")).expect("read generated Cargo.toml");
    assert!(
        cargo_toml.contains("path ="),
        "Cargo.toml must contain path override"
    );

    // 2. Run `cargo fmt --check` in the generated project
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

    // 3. Run `cargo check` in the generated project
    let check_ok = run_command(
        Command::new("cargo")
            .arg("check")
            .current_dir(&project_path),
    );
    assert!(check_ok, "`cargo check` should pass in generated project");

    // 4. Run `cargo test` in the generated project
    let test_ok = run_command(Command::new("cargo").arg("test").current_dir(&project_path));
    assert!(test_ok, "`cargo test` should pass in generated project");

    // 5. Exercise ALL supported generators:
    //    handler, resource, middleware, model, service
    for (kind, name) in [
        ("handler", "comments"),
        ("resource", "articles"),
        ("middleware", "auth"),
        ("model", "user"),
        ("service", "billing"),
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

    // 6. Verify conflict detection (C4): generating an existing component fails safely
    let duplicate_status = Command::new(rume_binary())
        .arg("generate")
        .arg("model")
        .arg("user")
        .current_dir(&project_path)
        .status()
        .expect("run duplicate generate");
    assert!(
        !duplicate_status.success(),
        "Generating existing model should fail with conflict error"
    );

    // 7. Check that all generated components compile cleanly with no errors
    let check_after_gen = run_command(
        Command::new("cargo")
            .arg("check")
            .current_dir(&project_path),
    );
    assert!(
        check_after_gen,
        "`cargo check` should pass after running all generators"
    );

    // 8. Run tests in the generated project after running all generators
    let test_after_gen = run_command(Command::new("cargo").arg("test").current_dir(&project_path));
    assert!(
        test_after_gen,
        "`cargo test` should pass after running all generators"
    );

    // 9. Verify CLI Command Contracts & JSON output (C5):
    //    a. `rume list:routes --json`
    let list_routes_output = Command::new(rume_binary())
        .arg("list:routes")
        .arg("--json")
        .current_dir(&project_path)
        .output()
        .expect("execute list:routes --json");
    if !list_routes_output.status.success() {
        panic!(
            "`rume list:routes --json` failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&list_routes_output.stdout),
            String::from_utf8_lossy(&list_routes_output.stderr)
        );
    }
    let routes_json = String::from_utf8_lossy(&list_routes_output.stdout);
    let parsed_routes: serde_json::Value =
        serde_json::from_str(&routes_json).expect("list:routes --json must return valid JSON");
    assert!(
        parsed_routes.is_array(),
        "list:routes JSON output must be an array"
    );
    let routes_arr = parsed_routes.as_array().unwrap();
    // Check that generated routes are present in the runtime route registry
    let paths: Vec<&str> = routes_arr
        .iter()
        .filter_map(|r| r.get("path").and_then(|p| p.as_str()))
        .collect();
    assert!(
        paths.contains(&"/comments"),
        "Route registry must contain /comments from handler generator"
    );
    assert!(
        paths.contains(&"/articles"),
        "Route registry must contain /articles from resource generator"
    );

    //    b. `rume doctor --format json`
    let doctor_output = Command::new(rume_binary())
        .arg("doctor")
        .arg("--format")
        .arg("json")
        .current_dir(&project_path)
        .output()
        .expect("execute doctor --format json");
    if !doctor_output.status.success() {
        panic!(
            "`rume doctor --format json` failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&doctor_output.stdout),
            String::from_utf8_lossy(&doctor_output.stderr)
        );
    }
    let doctor_json = String::from_utf8_lossy(&doctor_output.stdout);
    let parsed_doctor: serde_json::Value =
        serde_json::from_str(&doctor_json).expect("doctor JSON must be valid");
    assert!(
        parsed_doctor.get("checks").is_some(),
        "doctor JSON must have 'checks' field"
    );

    //    c. `rume env check --format json`
    let env_output = Command::new(rume_binary())
        .arg("env")
        .arg("check")
        .arg("--format")
        .arg("json")
        .current_dir(&project_path)
        .output()
        .expect("execute env check --format json");
    if !env_output.status.success() {
        panic!(
            "`rume env check --format json` failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&env_output.stdout),
            String::from_utf8_lossy(&env_output.stderr)
        );
    }
    let env_json = String::from_utf8_lossy(&env_output.stdout);
    let parsed_env: serde_json::Value =
        serde_json::from_str(&env_json).expect("env check JSON must be valid");
    assert!(
        parsed_env.get("resolved").is_some(),
        "env check JSON must have 'resolved' field"
    );

    //    d. `rume config check --format json`
    let config_output = Command::new(rume_binary())
        .arg("config")
        .arg("check")
        .arg("--format")
        .arg("json")
        .current_dir(&project_path)
        .output()
        .expect("execute config check --format json");
    if !config_output.status.success() {
        panic!(
            "`rume config check --format json` failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&config_output.stdout),
            String::from_utf8_lossy(&config_output.stderr)
        );
    }
    let config_json = String::from_utf8_lossy(&config_output.stdout);
    let parsed_config: serde_json::Value =
        serde_json::from_str(&config_json).expect("config check JSON must be valid");
    assert!(
        parsed_config.get("config").is_some(),
        "config check JSON must have 'config' field"
    );

    //    e. Standardized --json flag across commands:
    //       - rume doctor --json
    let doctor_json_flag = Command::new(rume_binary())
        .arg("doctor")
        .arg("--json")
        .current_dir(&project_path)
        .output()
        .expect("execute doctor --json");
    assert!(doctor_json_flag.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&doctor_json_flag.stdout)
        .expect("doctor --json output must be valid JSON");
    assert!(parsed.get("checks").is_some());

    //       - rume env check --json
    let env_json_flag = Command::new(rume_binary())
        .arg("env")
        .arg("check")
        .arg("--json")
        .current_dir(&project_path)
        .output()
        .expect("execute env check --json");
    assert!(env_json_flag.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&env_json_flag.stdout)
        .expect("env check --json output must be valid JSON");
    assert!(parsed.get("resolved").is_some());

    //       - rume config check --json
    let config_json_flag = Command::new(rume_binary())
        .arg("config")
        .arg("check")
        .arg("--json")
        .current_dir(&project_path)
        .output()
        .expect("execute config check --json");
    assert!(config_json_flag.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&config_json_flag.stdout)
        .expect("config check --json output must be valid JSON");
    assert!(parsed.get("config").is_some());

    //       - rume config explain app.name --json
    let explain_json = Command::new(rume_binary())
        .arg("config")
        .arg("explain")
        .arg("app.name")
        .arg("--json")
        .current_dir(&project_path)
        .output()
        .expect("execute config explain app.name --json");
    assert!(explain_json.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&explain_json.stdout)
        .expect("config explain --json output must be valid JSON");
    assert_eq!(parsed.get("key").and_then(|k| k.as_str()), Some("app.name"));
}

#[test]
fn make_migration_and_db_commands_workflow() {
    let tmp_dir = TempDir::new().expect("failed to create temp directory");
    let project_name = "test-migrations-app";

    // 1. Scaffold a project
    let status = Command::new(rume_binary())
        .arg("new")
        .arg(project_name)
        .arg("--path")
        .arg(framework_path())
        .current_dir(tmp_dir.path())
        .status()
        .expect("scaffold project");
    assert!(status.success());

    let project_dir = tmp_dir.path().join(project_name);

    // 2. Run `rume make:migration create_users_table`
    let make_res = Command::new(rume_binary())
        .arg("make:migration")
        .arg("create_users_table")
        .current_dir(&project_dir)
        .output()
        .expect("run make:migration");
    assert!(
        make_res.status.success(),
        "make:migration should exit with code 0"
    );

    let stdout = String::from_utf8_lossy(&make_res.stdout);
    assert!(stdout.contains("Created migrations/"));
    assert!(stdout.contains("create_users_table.up.sql"));
    assert!(stdout.contains("create_users_table.down.sql"));

    let migrations_dir = project_dir.join("migrations");
    assert!(migrations_dir.is_dir());

    let entries: Vec<_> = fs::read_dir(&migrations_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries.len(), 4);
    let up_file = entries
        .iter()
        .find(|n| n.contains("create_users_table") && n.ends_with(".up.sql"))
        .unwrap();
    let down_file = entries
        .iter()
        .find(|n| n.contains("create_users_table") && n.ends_with(".down.sql"))
        .unwrap();

    // 3. Run `rume db status` (offline / without DB URL)
    let status_offline = Command::new(rume_binary())
        .arg("db")
        .arg("status")
        .current_dir(&project_dir)
        .output()
        .expect("run db status");
    assert!(status_offline.status.success());
    let status_stdout = String::from_utf8_lossy(&status_offline.stdout);
    assert!(status_stdout.contains("create_users_table"));

    // 4. Configure local SQLite database
    let db_path = project_dir.join("test.db");
    let db_url = format!("sqlite://{}", db_path.display());

    // Write real migration DDL
    fs::write(
        migrations_dir.join(up_file),
        "CREATE TABLE test_users (id TEXT PRIMARY KEY, name TEXT NOT NULL);\n",
    )
    .unwrap();
    fs::write(migrations_dir.join(down_file), "DROP TABLE test_users;\n").unwrap();

    // 5. Run `rume db:migrate` with DATABASE_URL
    let migrate_res = Command::new(rume_binary())
        .arg("db:migrate")
        .env("DATABASE_URL", &db_url)
        .current_dir(&project_dir)
        .output()
        .expect("run db:migrate");
    assert!(migrate_res.status.success(), "db:migrate should succeed");
    let migrate_stdout = String::from_utf8_lossy(&migrate_res.stdout);
    assert!(migrate_stdout.contains("Successfully applied"));

    // 6. Run `rume db:status` and verify it is Applied
    let status_applied = Command::new(rume_binary())
        .arg("db:status")
        .env("DATABASE_URL", &db_url)
        .current_dir(&project_dir)
        .output()
        .expect("run db:status");
    assert!(status_applied.status.success());
    let status_applied_out = String::from_utf8_lossy(&status_applied.stdout);
    assert!(status_applied_out.contains("Applied"));

    // 7. Run `rume db:rollback` and verify rollback
    let rollback_res = Command::new(rume_binary())
        .arg("db:rollback")
        .env("DATABASE_URL", &db_url)
        .current_dir(&project_dir)
        .output()
        .expect("run db:rollback");
    assert!(rollback_res.status.success(), "db:rollback should succeed");
    let rollback_out = String::from_utf8_lossy(&rollback_res.stdout);
    assert!(rollback_out.contains("Rolled back"));
}
