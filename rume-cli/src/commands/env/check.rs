pub fn run() {
    let env_name = rume::env::resolve_env_name();
    println!("environment: {env_name}");

    match rume::env::ResolvedEnv::load() {
        Ok(resolved) => {
            if resolved.values.is_empty() {
                println!("(no environment variables resolved)");
            } else {
                println!("resolved environment:");
                for (key, value) in &resolved.values {
                    let source = resolved
                        .sources
                        .get(key)
                        .map(String::as_str)
                        .unwrap_or("unknown");
                    let display = rume::env::redact(key, value);
                    println!("  {key}={display}  ({source})");
                }
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
