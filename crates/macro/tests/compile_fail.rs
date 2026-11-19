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

#[test]
fn compile_fail_ui_tests() {
    // Why: trybuild compiles UI test crates inside `target/tests/trybuild/rume-macros`
    // with that directory as `CARGO_MANIFEST_DIR`. Copying `tests/fixtures` into
    // trybuild's manifest dir keeps the proc-macro strictly confined to
    // `CARGO_MANIFEST_DIR` without any unsafe fallback or escaping symlinks.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target"));
    let trybuild_fixtures = workspace_target.join("tests/trybuild/rume-macros/tests/fixtures");
    let _ = fs::remove_dir_all(&trybuild_fixtures);
    copy_dir_recursive(&manifest_dir.join("tests/fixtures"), &trybuild_fixtures);

    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
