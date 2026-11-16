use crate::util::{is_tool_available, run};

/// Run `cargo audit` security advisory check against `Cargo.lock` (respecting `.cargo/audit.toml`).
pub fn run_audit(strict: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !is_tool_available("cargo-audit") {
        if strict {
            return Err(
                "cargo-audit is not installed (install with `cargo install cargo-audit`)".into(),
            );
        }
        println!("\x1b[33m! skipping cargo-audit (not installed)\x1b[0m");
        return Ok(());
    }
    run("cargo", &["audit"])
}
