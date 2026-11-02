use rume::config;
use rume::env;
use serde::Serialize;

#[derive(Serialize)]
struct DoctorOutput {
    all_passed: bool,
    checks: Vec<DoctorCheck>,
}

#[derive(Serialize)]
struct DoctorCheck {
    label: String,
    status: String,
    ok: bool,
}

pub fn run(format: &str) {
    let mut ok = true;
    let mut checks: Vec<DoctorCheck> = Vec::new();

    // Rust toolchain
    let toolchain_result = match std::process::Command::new("rustc")
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
    };
    let toolchain_ok = toolchain_result.starts_with("rustc ");
    checks.push(DoctorCheck {
        label: "rust toolchain".to_string(),
        status: toolchain_result,
        ok: toolchain_ok,
    });

    // Environment
    let env_result = match env::ResolvedEnv::load() {
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
    };
    let env_ok = !env_result.starts_with("error:");
    checks.push(DoctorCheck {
        label: "environment files".to_string(),
        status: env_result,
        ok: env_ok,
    });

    // Configuration
    let config_result = match config::load_raw(std::path::Path::new(".")) {
        Ok(value) => format!(
            "merged config loaded ({} top-level key(s))",
            value.as_table().map(|t| t.len()).unwrap_or(0)
        ),
        Err(err) => {
            ok = false;
            format!("error: {err}")
        }
    };
    let config_ok = !config_result.starts_with("error:");
    checks.push(DoctorCheck {
        label: "configuration".to_string(),
        status: config_result,
        ok: config_ok,
    });

    // .env example
    let env_example_result = if std::path::Path::new(".env.example").is_file() {
        "present".to_string()
    } else {
        "missing (recommended)".to_string()
    };
    checks.push(DoctorCheck {
        label: ".env.example".to_string(),
        status: env_example_result,
        ok: true,
    });

    // Port availability
    let port_result = match env::ResolvedEnv::load() {
        Ok(resolved) => {
            let port = resolved
                .get("PORT")
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(3333);
            let host = resolved
                .get("HOST")
                .and_then(|value| value.parse::<std::net::IpAddr>().ok())
                .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));

            match std::net::TcpListener::bind((host, port)) {
                Ok(_) => format!("port {port} is available"),
                Err(err) => {
                    ok = false;
                    format!("port {port} is in use: {err} (stop the process or change PORT)")
                }
            }
        }
        Err(err) => {
            ok = false;
            format!("error: {err}")
        }
    };
    let port_ok = port_result.starts_with("port ");
    checks.push(DoctorCheck {
        label: "port availability".to_string(),
        status: port_result,
        ok: port_ok,
    });

    let output = DoctorOutput {
        all_passed: ok,
        checks,
    };

    match format {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&output).unwrap_or_default()
            );
        }
        _ => {
            println!("Rume doctor");
            println!();

            for check in &output.checks {
                let status = if check.ok { "ok" } else { "FAIL" };
                println!("  [{}] {} ({})", check.label, check.status, status);
            }

            println!();
            if ok {
                println!("All checks passed.");
            } else {
                println!("Some checks failed.");
                std::process::exit(1);
            }
        }
    }
}
