use crate::commands::{
    audit::run_audit, doc::run_doc, features::run_features, quick::run_quick, test::run_test,
};

/// Run the full pre-commit / CI verification pipeline for Rume.
pub fn run_ci(strict: bool, containers: bool) -> Result<(), Box<dyn std::error::Error>> {
    run_quick()?;
    run_doc()?;
    run_audit(strict)?;
    run_features()?;
    run_test(containers)?;
    println!("\x1b[32m✓ All Rume CI checks passed\x1b[0m");
    Ok(())
}
