use rume::config;

pub fn run(key: &str, json: bool) {
    let source_map = match config::source_map(std::path::Path::new(".")) {
        Ok(sm) => sm,
        Err(err) => {
            if json {
                let err_json = serde_json::json!({ "error": err.to_string() });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&err_json).unwrap_or_default()
                );
            } else {
                eprintln!("error: failed to load configuration: {err}");
            }
            std::process::exit(1);
        }
    };

    let explanation = source_map.explain(key);
    match &explanation.value {
        Some(val) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&explanation).unwrap_or_default()
                );
            } else {
                println!("KEY: {}", explanation.key);
                println!("Resolved value: {val}");
                let file_str = explanation
                    .winning_file
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                println!("Source: {file_str}");
                println!("Precedence hierarchy:");
                for candidate in &explanation.candidate_precedence {
                    println!("  - {}", candidate.display());
                }
            }
        }
        None => {
            if json {
                let err_json = serde_json::json!({
                    "error": format!("configuration key '{key}' is not defined")
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&err_json).unwrap_or_default()
                );
            } else {
                eprintln!("error: configuration key '{key}' is not defined");
                eprintln!("hint: check your config/app.toml or environment variables");
            }
            std::process::exit(1);
        }
    }
}
