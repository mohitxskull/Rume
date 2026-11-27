use crate::util::{is_tool_available, run};

/// Run `cargo audit` and `cargo deny check` against `Cargo.lock` and workspace policies.
pub fn run_audit(strict: bool) -> Result<(), Box<dyn std::error::Error>> {
    if is_tool_available("cargo-audit") {
        run("cargo", &["audit"])?;
    } else if strict {
        return Err(
            "cargo-audit is not installed (install with `cargo install cargo-audit`)".into(),
        );
    } else {
        println!("\x1b[33m! skipping cargo-audit (not installed)\x1b[0m");
    }

    if is_tool_available("cargo-deny") {
        run("cargo", &["deny", "check"])?;
    } else if strict {
        return Err("cargo-deny is not installed (install with `cargo install cargo-deny`)".into());
    } else {
        println!("\x1b[33m! skipping cargo-deny (not installed)\x1b[0m");
    }

    Ok(())
}
