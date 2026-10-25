pub fn run(all: bool) {
    let env_name = rume::env::resolve_env_name();
    println!("environment: {env_name}");

    match rume::env::ResolvedEnv::load() {
        Ok(resolved) => {
            let mut shown = 0usize;
            let mut hidden = 0usize;

            if resolved.values.is_empty() {
                println!("(no environment variables resolved)");
                return;
            }

            println!("resolved environment:");

            for (key, value) in &resolved.values {
                let source = resolved
                    .sources
                    .get(key)
                    .map(String::as_str)
                    .unwrap_or("unknown");

                if !all && source == "process env" {
                    hidden += 1;
                    continue;
                }

                shown += 1;
                let display = rume::env::redact(key, value);
                println!("  {key}={display}  ({source})");
            }

            if shown == 0 {
                println!("  (no file-defined variables)");
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
