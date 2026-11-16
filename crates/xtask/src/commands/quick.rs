use crate::util::run;

/// Fast inner loop: format check, strict clippy across all targets/features, and core lib unit tests.
pub fn run_quick() -> Result<(), Box<dyn std::error::Error>> {
    run("cargo", &["fmt", "--all", "--check"])?;
    run(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run("cargo", &["test", "-p", "rume", "--lib"])?;
    Ok(())
}
