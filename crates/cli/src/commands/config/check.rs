use rume::config;
use rume::env::redact_value;
use serde::Serialize;

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

/// Recursively redact secret-looking values in a toml::Value AST before serialization.
///
/// Operating on the structured AST eliminates multi-line string leakage (CLI C1),
/// ensuring multi-line RSA and PEM private keys, certificates, secret-keyed arrays,
/// and credential URLs inside arrays are redacted in both human-readable and JSON modes.
pub fn redact_toml_value(value: &mut toml::Value) {
    match value {
        toml::Value::Table(table) => {
            for (key, val) in table.iter_mut() {
                let key_is_secret = rume::env::is_secret_key(key);
                let scalar_redacted = toml_value_as_string(val)
                    .map(|v| redact_value(key, &v) == "[redacted]")
                    .unwrap_or(false);
                if scalar_redacted {
                    *val = toml::Value::String("[redacted]".to_string());
                } else if key_is_secret {
                    // Why: a secret-named key holding an array (e.g. `api_keys = ["sk_live_..."]`)
                    // or table must redact every nested scalar element rather than bypassing
                    // redaction because `toml_value_as_string` returns `None` for containers.
                    redact_all_scalars(val);
                } else {
                    redact_toml_value(val);
                }
            }
        }
        toml::Value::Array(items) => {
            for item in items.iter_mut() {
                if let Some(s) = toml_value_as_string(item)
                    && redact_value("", &s) == "[redacted]"
                {
                    *item = toml::Value::String("[redacted]".to_string());
                } else {
                    redact_toml_value(item);
                }
            }
        }
        _ => {}
    }
}

fn redact_all_scalars(value: &mut toml::Value) {
    match value {
        toml::Value::Table(table) => {
            for (_, v) in table.iter_mut() {
                redact_all_scalars(v);
            }
        }
        toml::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_all_scalars(item);
            }
        }
        _ => {
            *value = toml::Value::String("[redacted]".to_string());
        }
    }
}

#[derive(Serialize)]
struct ConfigCheckOutput {
    config: toml::Value,
}

pub fn run(unredacted: bool, format: &str) {
    match config::load_raw(std::path::Path::new(".")) {
        Ok(value) => {
            let mut output_value = value;
            if !unredacted {
                redact_toml_value(&mut output_value);
            }
            match format {
                "json" => {
                    let output = ConfigCheckOutput {
                        config: output_value,
                    };
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).unwrap_or_default()
                    );
                }
                _ => {
                    let text = toml::to_string_pretty(&output_value).unwrap_or_default();
                    println!("{text}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_toml_value_multiline_secrets() {
        let raw = r#"
            app_name = "demo"
            private_key = """
-----BEGIN RSA PRIVATE KEY-----
MIIEogIBAAKCAQEA0+abc...
-----END RSA PRIVATE KEY-----
"""
            database_url = "postgres://admin:secretpass@localhost:5432/db"
            safe_setting = "hello world"
        "#;
        let mut val: toml::Value = toml::from_str(raw).unwrap();
        redact_toml_value(&mut val);

        let formatted = toml::to_string_pretty(&val).unwrap();
        assert!(!formatted.contains("BEGIN RSA PRIVATE KEY"));
        assert!(!formatted.contains("secretpass"));
        assert!(formatted.contains("private_key = \"[redacted]\""));
        assert!(formatted.contains("database_url = \"[redacted]\""));
        assert!(formatted.contains("safe_setting = \"hello world\""));
    }
}
