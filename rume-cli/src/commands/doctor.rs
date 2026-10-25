use rume::config;
use rume::env;

pub fn run() {
    println!("Rume doctor");
    println!();

    let mut ok = true;

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
