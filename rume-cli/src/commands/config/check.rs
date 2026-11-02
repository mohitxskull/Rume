use rume::config;
use rume::env::redact_value;
use serde::Serialize;

/// Extract the inner string content of a TOML value token, for redaction
/// decisions. Quoted strings are unquoted; unquoted scalars are taken as-is.
/// Containers return `None` and are recursed into instead.
fn toml_value_inner(trimmed: &str) -> Option<String> {
    if let Some(inner) = trimmed.strip_prefix('"').and_then(|q| q.strip_suffix('"')) {
        return Some(inner.to_string());
    }
    if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|q| q.strip_suffix('\''))
    {
        return Some(inner.to_string());
    }
    if !trimmed.is_empty() && !trimmed.starts_with('[') && !trimmed.starts_with('{') {
        return Some(trimmed.to_string());
    }
    None
}

/// Redact secret-looking values in a single `key = "value"` TOML line.
///
/// Redaction uses [`rume::env::redact_value`], which matches on both the key
/// and the value, so a credential URL is redacted even when the key does not
/// match the secret heuristics.
fn redact_toml_line(line: &str) -> String {
    // Match lines like:  key = "value"   or   key = 'value'
    if let Some((key, rest)) = line.split_once('=') {
        let key = key.trim();
        let trimmed = rest.trim();
        let value = match toml_value_inner(trimmed) {
            Some(v) => v,
            None => return line.to_string(),
        };
        if redact_value(key, &value) == "[redacted]" {
            // Preserve the quoting style of the original value.
            if trimmed.starts_with('"') && trimmed.ends_with('"') {
                return format!("{key} = \"[redacted]\"");
            }
            if trimmed.starts_with('\'') && trimmed.ends_with('\'') {
                return format!("{key} = '[redacted]'");
            }
            // Unquoted value (number, bool, etc.)
            return format!("{key} = [redacted]");
        }
    }
    line.to_string()
}

/// Render a scalar toml::Value as its string content, for redaction decisions.
/// Containers (arrays/tables) return `None` and are recursed into instead.
fn toml_value_as_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Recursively redact secret-looking values in a toml::Value.
///
/// Uses [`rume::env::redact_value`] so both key-based and value-based
/// detection apply consistently with the rest of the application.
fn redact_toml_value(value: &mut toml::Value) {
    match value {
        toml::Value::Table(table) => {
            let keys: Vec<String> = table.keys().cloned().collect();
            for key in keys {
                let redacted = table
                    .get(&key)
                    .and_then(toml_value_as_string)
                    .map(|v| redact_value(&key, &v) == "[redacted]")
                    .unwrap_or(false);
                if redacted {
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
