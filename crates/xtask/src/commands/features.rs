use crate::util::run;

/// Verify isolated backend feature combinations for `rume`.
pub fn run_features() -> Result<(), Box<dyn std::error::Error>> {
    let combinations: &[&[&str]] = &[
        &[
            "check",
            "-p",
            "rume",
            "--no-default-features",
            "--features",
            "sqlite,testing",
        ],
        &[
            "check",
            "-p",
            "rume",
            "--no-default-features",
            "--features",
            "postgres,testing",
        ],
        &[
            "check",
            "-p",
            "rume",
            "--no-default-features",
            "--features",
            "mysql,testing",
        ],
        &[
            "check",
            "-p",
            "rume",
            "--no-default-features",
            "--features",
            "redis,testing",
        ],
        &["check", "-p", "rume", "--all-features"],
    ];
    for args in combinations {
        run("cargo", args)?;
    }
    Ok(())
}
