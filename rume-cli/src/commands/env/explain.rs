//! Explain how a single environment variable was resolved.
//!
//! Shows the resolved value (redacted if secret), source, and provenance
//! for a given environment variable key.

use rume::env::{ResolvedEnv, is_secret_key, redact_value};

pub fn run(key: &str) {
    let resolved = match ResolvedEnv::load() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("error: failed to load environment: {err}");
            std::process::exit(1);
        }
    };

    // Look up the value (secrets are exposed internally via `get`).
    let raw_value = match resolved.get(key) {
        Some(v) => v.to_string(),
        None => {
            eprintln!("error: environment variable '{key}' is not defined");
            eprintln!("hint: check your .env files or process environment");
            std::process::exit(1);
        }
    };

    let is_secret = is_secret_key(key);
    let display_value = redact_value(key, &raw_value);

    let source = resolved
        .sources
        .get(key)
        .map(String::as_str)
        .unwrap_or("unknown");

    let from_file = resolved.file_keys.contains(key);
    let source_type = if from_file { "env file" } else { "process env" };

    println!("KEY: {key}");
    println!("Resolved value: {display_value}");
    println!("Type: string");
    println!("Status: Valid");
    println!();
    println!("Sources:");
    println!(
        "  {}: {}  ({})",
        source,
        truncate_value(&raw_value, 60),
        source_type
    );
    if !from_file {
        println!("  (not set in any .env file)");
    }
    println!();
    println!("Winning source:");
    println!("  {}", source);
    println!();
    println!("Precedence:");
    println!("  Highest priority wins.");
    if is_secret {
        println!();
        println!("Note: this value is treated as a secret and is redacted in output.");
    }
}

/// Truncate a value for display, adding "..." if it exceeds max_len.
fn truncate_value(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        value.to_string()
    } else {
        format!("{}...", &value[..max_len.saturating_sub(3)])
    }
}
