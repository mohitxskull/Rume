//! Shared test helpers: locate workspace members on disk via `cargo_metadata`.
//!
//! Why `cargo_metadata` rather than `CARGO_MANIFEST_DIR` + `../../crates/core`:
//! the latter hard-codes the directory depth and breaks silently if the
//! workspace layout changes (e.g. `crates/core` moved to `framework/core`).
//! `cargo_metadata` asks Cargo itself where package `rume` lives, so the
//! path stays correct no matter how crates are reorganized.

use std::path::PathBuf;

/// Returns the absolute path to the `rume` (core framework) crate directory.
///
/// Panics if the workspace metadata cannot be read or the `rume` package is
/// not found — both are programming errors in the test setup, not runtime
/// conditions.
pub fn framework_path() -> PathBuf {
    let metadata = cargo_metadata::MetadataCommand::new()
        .exec()
        .expect("failed to read workspace Cargo metadata");
    let package = metadata
        .packages
        .iter()
        .find(|p| p.name == "rume")
        .expect("workspace member `rume` not found in Cargo metadata");
    package
        .manifest_path
        .parent()
        .expect("rume manifest_path should have a parent directory")
        .as_std_path()
        .to_path_buf()
}
