use crate::util::run;

/// Run workspace unit, integration, and (optionally) container tests.
pub fn run_test(containers: bool) -> Result<(), Box<dyn std::error::Error>> {
    run("cargo", &["test", "--workspace", "--lib"])?;
    run("cargo", &["test", "-p", "rume", "--test", "integration"])?;
    run("cargo", &["test", "-p", "rume-cli"])?;
    run("cargo", &["test", "-p", "rume-macros"])?;
    if containers {
        run(
            "cargo",
            &[
                "test",
                "-p",
                "rume",
                "--test",
                "containers",
                "--all-features",
            ],
        )?;
    }
    Ok(())
}
