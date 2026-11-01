use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
struct EnvCheckOutput {
    environment: String,
    resolved: Vec<EnvVarEntry>,
    process: Vec<EnvVarEntry>,
    hidden_count: usize,
}

#[derive(Serialize)]
struct EnvVarEntry {
    key: String,
    value: String,
    source: String,
}

pub fn run(all: bool, unredacted: bool, format: &str) {
    let env_name = rume::env::resolve_env_name();

    match rume::env::ResolvedEnv::load() {
        Ok(resolved) => {
            let mut output = EnvCheckOutput {
                environment: env_name,
                resolved: Vec::new(),
                process: Vec::new(),
                hidden_count: 0,
            };

            // Collect all file-sourced keys (both values and secrets).
            let mut file_keys: BTreeSet<String> = BTreeSet::new();
            for key in &resolved.file_keys {
                file_keys.insert(key.clone());
            }
            for key in resolved.secrets.keys() {
                file_keys.insert(key.clone());
            }

            for key in file_keys {
                let value = resolved.get(&key).unwrap_or("");
                let source = resolved
                    .sources
                    .get(&key)
                    .map(String::as_str)
                    .unwrap_or("unknown");
                let display = if unredacted {
                    value.to_string()
                } else {
                    rume::env::redact(&key, value)
                };
                output.resolved.push(EnvVarEntry {
                    key,
                    value: display,
                    source: source.to_string(),
                });
            }

            if all {
                // Show process-env-only variables (not from .env files).
                let mut process_keys: BTreeSet<String> = BTreeSet::new();
                for key in resolved.values.keys() {
                    if !resolved.file_keys.contains(key.as_str()) {
                        process_keys.insert(key.clone());
                    }
                }
                for key in resolved.secrets.keys() {
                    if !resolved.file_keys.contains(key.as_str()) {
                        process_keys.insert(key.clone());
                    }
                }
                for key in process_keys {
                    let value = resolved.get(&key).unwrap_or("");
                    let display = if unredacted {
                        value.to_string()
                    } else {
                        rume::env::redact(&key, value)
                    };
                    output.process.push(EnvVarEntry {
                        key,
                        value: display,
                        source: "process env".to_string(),
                    });
                }
            } else {
                let process_count = resolved
                    .values
                    .len()
                    .saturating_sub(resolved.file_keys.len())
                    + resolved
                        .secrets
                        .len()
                        .saturating_sub(resolved.file_keys.len());
                output.hidden_count = process_count;
            }

            match format {
                "json" => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_default()
                    );
                }
                _ => {
                    println!("environment: {}", output.environment);

                    if output.resolved.is_empty() && output.process.is_empty() && !all {
                        println!("(no environment variables resolved from .env files)");
                    }

                    if !output.resolved.is_empty() {
                        use crate::ui::table::TableBuilder;
                        println!("resolved environment:");
                        let mut table = TableBuilder::new();
                        table.head(&["Variable", "Value", "Source"]);
                        for entry in &output.resolved {
                            table.row(&[&entry.key, &entry.value, &entry.source]);
                        }
                        table.render();
                    }

                    if all && !output.process.is_empty() {
                        println!("process environment:");
                        for entry in &output.process {
                            println!("  {}={}  (process env)", entry.key, entry.value);
                        }
                    }

                    if output.hidden_count > 0 {
                        println!(
                            "\n  {} process environment variable(s) hidden. \
                             Use `rume env check --all` to show them.",
                            output.hidden_count
                        );
                    }
                }
            }
        }
        Err(err) => {
            if format == "json" {
                let err_json = serde_json::json!({"error": err.to_string()});
                println!(
                    "{}",
                    serde_json::to_string_pretty(&err_json).unwrap_or_default()
                );
            } else {
                eprintln!("error: {err}");
            }
            std::process::exit(1);
        }
    }
}
