use std::collections::BTreeSet;

pub fn run(all: bool, unredacted: bool) {
    let env_name = rume::env::resolve_env_name();
    println!("environment: {env_name}");

    match rume::env::ResolvedEnv::load() {
        Ok(resolved) => {
            let mut shown = 0usize;
            let mut hidden = 0usize;

            if resolved.file_keys.is_empty() && resolved.secrets.is_empty() && !all {
                println!("(no environment variables resolved from .env files)");
                // fall through to show process env if --all
            }

            if !resolved.file_keys.is_empty() || !resolved.secrets.is_empty() {
                use crate::ui::table::TableBuilder;

                println!("resolved environment:");
                let mut table = TableBuilder::new();
                table.head(&["Variable", "Value", "Source"]);

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

                    shown += 1;
                    let display = if unredacted {
                        value.to_string()
                    } else {
                        rume::env::redact(&key, value)
                    };
                    table.row(&[&key, &display, source]);
                }
                table.render();
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
                if !process_keys.is_empty() {
                    println!("process environment:");
                    for key in process_keys {
                        let value = resolved.get(&key).unwrap_or("");
                        let display = if unredacted {
                            value.to_string()
                        } else {
                            rume::env::redact(&key, value)
                        };
                        println!("  {key}={display}  (process env)");
                        shown += 1;
                    }
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
                if process_count > 0 {
                    hidden += process_count;
                }
            }

            if shown == 0 {
                println!("  (no variables)");
            }

            if hidden > 0 {
                println!(
                    "\n  {hidden} process environment variable(s) hidden. \
                     Use `rume env check --all` to show them."
                );
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
