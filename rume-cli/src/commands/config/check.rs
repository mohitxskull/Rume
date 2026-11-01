use rume::config;
use rume::env::is_secret_key;
use serde::Serialize;

/// Redact secret-looking values in a single `key = "value"` TOML line.
fn redact_toml_line(line: &str) -> String {
    // Match lines like:  key = "value"   or   key = 'value'
    if let Some((key, rest)) = line.split_once('=') {
        let key = key.trim();
        if is_secret_key(key) {
            // Preserve the quoting style of the original value.
            let trimmed = rest.trim();
            if let Some(quoted) = trimmed.strip_prefix('"')
                && let Some(_inner) = quoted.strip_suffix('"')
            {
                return format!("{key} = \"[redacted]\"");
            }
            if let Some(quoted) = trimmed.strip_prefix('\'')
                && let Some(_inner) = quoted.strip_suffix('\'')
            {
                return format!("{key} = '[redacted]'");
            }
            // Unquoted value (number, bool, etc.)
            return format!("{key} = [redacted]");
        }
    }
    line.to_string()
}

/// Recursively redact secret-looking keys in a toml::Value.
fn redact_toml_value(value: &mut toml::Value) {
    match value {
        toml::Value::Table(table) => {
            let keys: Vec<String> = table.keys().cloned().collect();
            for key in keys {
                if is_secret_key(&key) {
                    table.insert(key, toml::Value::String("[redacted]".to_string()));
                } else {
                    redact_toml_value(table.get_mut(&key).unwrap());
                }
            }
        }
        toml::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_toml_value(item);
            }
        }
        _ => {}
    }
}

#[derive(Serialize)]
struct ConfigCheckOutput {
    config: toml::Value,
}

pub fn run(unredacted: bool, format: &str) {
    match config::load_raw(std::path::Path::new(".")) {
        Ok(value) => match format {
            "json" => {
                let mut output_value = value.clone();
                if !unredacted {
                    redact_toml_value(&mut output_value);
                }
                let output = ConfigCheckOutput {
                    config: output_value,
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).unwrap_or_default()
                );
            }
            _ => {
                let text = toml::to_string_pretty(&value).unwrap_or_default();
                if unredacted {
                    println!("{text}");
                } else {
                    let redacted: String = text
                        .lines()
                        .map(redact_toml_line)
                        .collect::<Vec<_>>()
                        .join("\n");
                    println!("{redacted}");
                }
            }
        },
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
