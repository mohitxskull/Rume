use rume::config;
use rume::env;

pub fn run() {
    println!("Rume doctor");
    println!();

    let mut ok = true;

    // Rust toolchain
    print_check("rust toolchain", || {
        match std::process::Command::new("rustc")
            .arg("--version")
            .output()
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => {
                ok = false;
                "rustc not found".to_string()
            }
        }
    });

    // Environment
    print_check("environment files", || match env::ResolvedEnv::load() {
        Ok(resolved) => {
            let file_vars = resolved
                .sources
                .values()
                .filter(|source| source.as_str() != "process env")
                .count();
            if file_vars == 0 {
                "no file-defined variables (process env only)".to_string()
            } else {
                format!("{file_vars} file-defined variable(s) resolved")
            }
        }
        Err(err) => {
            ok = false;
            format!("error: {err}")
        }
    });

    // Configuration
    print_check("configuration", || {
        match config::load_raw(std::path::Path::new(".")) {
            Ok(value) => format!(
                "merged config loaded ({} top-level key(s))",
                value.as_table().map(|t| t.len()).unwrap_or(0)
            ),
            Err(err) => {
                ok = false;
                format!("error: {err}")
            }
        }
    });

    // .env example
    print_check(".env.example", || {
        if std::path::Path::new(".env.example").is_file() {
            "present".to_string()
        } else {
            "missing (recommended)".to_string()
        }
    });

    // Port availability
    print_check("port availability", || {
        let port = std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(3333);

        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(_) => format!("port {port} is available"),
            Err(err) => {
                ok = false;
                format!("port {port} is in use: {err} (stop the process or change PORT)")
            }
        }
    });

    println!();
    if ok {
        println!("All checks passed.");
    } else {
        println!("Some checks failed.");
        std::process::exit(1);
    }
}

fn print_check<F>(label: &str, f: F)
where
    F: FnOnce() -> String,
{
    println!("  [{label}] {}", f());
}
