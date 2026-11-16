use crate::util::run;

/// Run workspace doc-tests and build `rume` rustdoc documentation.
pub fn run_doc() -> Result<(), Box<dyn std::error::Error>> {
    run("cargo", &["test", "--workspace", "--doc", "--all-features"])?;
    run(
        "cargo",
        &["doc", "-p", "rume", "--no-deps", "--all-features"],
    )?;
    Ok(())
}
