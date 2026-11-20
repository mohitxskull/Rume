use std::fs;
use std::path::{Path, PathBuf};

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create fixture dir");
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let ty = entry.file_type().expect("entry type");
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), dst_path).expect("copy fixture file");
        }
    }
}

fn resolve_target_directory() -> PathBuf {
    let output = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata JSON");
    PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .expect("cargo metadata target_directory"),
    )
}

#[test]
fn compile_fail_ui_tests() {
    // Why: trybuild compiles UI test crates inside `<target_directory>/tests/trybuild/rume-macros`
    // with that directory as `CARGO_MANIFEST_DIR`. Resolving `target_directory` via `cargo metadata`
    // matches trybuild 1.0.125 even when `build.target-dir` is configured in `.cargo/config.toml`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_target = resolve_target_directory();
    let trybuild_fixtures = workspace_target.join("tests/trybuild/rume-macros/tests/fixtures");
    let _ = fs::remove_dir_all(&trybuild_fixtures);
    copy_dir_recursive(&manifest_dir.join("tests/fixtures"), &trybuild_fixtures);

    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
