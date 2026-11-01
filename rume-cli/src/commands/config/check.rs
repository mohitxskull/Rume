use rume::env::is_secret_key;

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

pub fn run(unredacted: bool) {
    match rume::config::load_raw_pretty(std::path::Path::new(".")) {
        Ok(text) => {
            if unredacted {
                println!("{text}");
            } else {
                // Redact secret values by default; --unredacted shows everything.
                let redacted: String = text
                    .lines()
                    .map(redact_toml_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                println!("{redacted}");
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
