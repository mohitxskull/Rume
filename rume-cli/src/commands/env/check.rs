pub fn run(all: bool) {
    let env_name = rume::env::resolve_env_name();
    println!("environment: {env_name}");

    match rume::env::ResolvedEnv::load() {
        Ok(resolved) => {
            let mut shown = 0usize;
            let mut hidden = 0usize;

            if resolved.file_keys.is_empty() && !all {
                println!("(no environment variables resolved from .env files)");
                // fall through to show process env if --all
            }

            if !resolved.file_keys.is_empty() {
                use crate::ui::table::TableBuilder;

                println!("resolved environment:");
                let mut table = TableBuilder::new();
                table.head(&["Variable", "Value", "Source"]);
                for key in &resolved.file_keys {
                    let value = resolved.values.get(key).map(String::as_str).unwrap_or("");
                    let source = resolved
                        .sources
                        .get(key)
                        .map(String::as_str)
                        .unwrap_or("unknown");

                    shown += 1;
                    let display = rume::env::redact(key, value);
                    table.row(&[key, &display, source]);
                }
                table.render();
            }

            if all {
                // Show process-env-only variables.
                let mut process_only: Vec<&String> = resolved
                    .values
                    .keys()
                    .filter(|k| !resolved.file_keys.contains(*k))
                    .collect();
                process_only.sort();
                if !process_only.is_empty() {
                    println!("process environment:");
                    for key in process_only {
                        let value = resolved.values.get(key).map(String::as_str).unwrap_or("");
                        let display = rume::env::redact(key, value);
                        println!("  {key}={display}  (process env)");
                        shown += 1;
                    }
                }
            } else {
                let process_count = resolved
                    .values
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
