use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

pub async fn run(format: &str) {
    let mut ok = true;
    let mut checks: Vec<DoctorCheck> = Vec::new();

    // 1. Rust toolchain
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

    // 2. Environment files
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

    // 3. Configuration
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

    // 4. .env.example
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

    // 5. Port availability
    let (port_result, port_ok) = match env::ResolvedEnv::load() {
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
                Ok(_) => (format!("port {port} is available"), true),
                Err(err) => {
                    ok = false;
                    (
                        format!("port {port} is in use: {err} (stop the process or change PORT)"),
                        false,
                    )
                }
            }
        }
        Err(err) => {
            ok = false;
            (format!("error: {err}"), false)
        }
    };
    checks.push(DoctorCheck {
        label: "port availability".to_string(),
        status: port_result,
        ok: port_ok,
    });

    // 6. Database connectivity & 7. Pending migrations
    let (db_check, migrations_check) = check_database_and_migrations().await;
    if !db_check.ok {
        ok = false;
    }
    if !migrations_check.ok {
        ok = false;
    }
    checks.push(db_check);
    checks.push(migrations_check);

    // 8. Redis connectivity
    let redis_check = check_redis_connectivity();
    if !redis_check.ok {
        ok = false;
    }
    checks.push(redis_check);

    // 9. Module DAG acyclicity
    let (dag_status, dag_ok) = check_module_dag();
    if !dag_ok {
        ok = false;
    }
    checks.push(DoctorCheck {
        label: "module DAG acyclicity".to_string(),
        status: dag_status,
        ok: dag_ok,
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
            if !ok {
                std::process::exit(1);
            }
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

async fn check_database_and_migrations() -> (DoctorCheck, DoctorCheck) {
    let db_url = crate::commands::db::resolve_db_url();
    let Some(url) = db_url else {
        return (
            DoctorCheck {
                label: "database connectivity".to_string(),
                status: "DATABASE_URL not set (skipped)".to_string(),
                ok: true,
            },
            DoctorCheck {
                label: "pending migrations".to_string(),
                status: "database not configured (skipped)".to_string(),
                ok: true,
            },
        );
    };

    let conn_res = crate::commands::db::connect_backend(&url).await;
    match conn_res {
        Ok((handle, dialect)) => {
            use rume::persistence::DatabaseBackend;
            let ping_res = match &handle {
                crate::commands::db::BackendHandle::Postgres(b) => b.ping().await,
                crate::commands::db::BackendHandle::Sqlite(b) => b.ping().await,
                crate::commands::db::BackendHandle::Mysql(b) => b.ping().await,
            };

            let db_check = match ping_res {
                Ok(()) => DoctorCheck {
                    label: "database connectivity".to_string(),
                    status: format!("{dialect} connection healthy"),
                    ok: true,
                },
                Err(e) => DoctorCheck {
                    label: "database connectivity".to_string(),
                    status: format!("{dialect} ping failed: {e}"),
                    ok: false,
                },
            };

            let local_migrator = crate::commands::db::discover_local_migrations();
            let migrator = local_migrator.with_rume_core(dialect);
            let total_migrations = migrator.migrations().len();

            let applied_res = match &handle {
                crate::commands::db::BackendHandle::Postgres(b) => migrator.applied_migrations(b).await,
                crate::commands::db::BackendHandle::Sqlite(b) => migrator.applied_migrations(b).await,
                crate::commands::db::BackendHandle::Mysql(b) => migrator.applied_migrations(b).await,
            };

            let migrations_check = match applied_res {
                Ok(applied) => {
                    let applied_count = applied.len();
                    let pending = total_migrations.saturating_sub(applied_count);
                    if pending == 0 {
                        DoctorCheck {
                            label: "pending migrations".to_string(),
                            status: format!("all {total_migrations} migration(s) applied (up to date)"),
                            ok: true,
                        }
                    } else {
                        DoctorCheck {
                            label: "pending migrations".to_string(),
                            status: format!("{pending} pending migration(s) (run `rume db migrate`)"),
                            ok: true,
                        }
                    }
                }
                Err(e) => DoctorCheck {
                    label: "pending migrations".to_string(),
                    status: format!("warning: failed to inspect migration ledger: {e}"),
                    ok: true,
                },
            };

            (db_check, migrations_check)
        }
        Err(err) => (
            DoctorCheck {
                label: "database connectivity".to_string(),
                status: format!("failed to initialize backend: {err}"),
                ok: false,
            },
            DoctorCheck {
                label: "pending migrations".to_string(),
                status: "database unreachable (skipped)".to_string(),
                ok: true,
            },
        ),
    }
}

fn check_redis_connectivity() -> DoctorCheck {
    let redis_url = std::env::var("REDIS_URL").ok().or_else(|| {
        let _ = dotenvy::dotenv();
        std::env::var("REDIS_URL").ok()
    });

    let Some(url) = redis_url else {
        return DoctorCheck {
            label: "redis connectivity".to_string(),
            status: "REDIS_URL not set (skipped)".to_string(),
            ok: true,
        };
    };

    let trimmed = url.trim();
    if trimmed.is_empty() {
        return DoctorCheck {
            label: "redis connectivity".to_string(),
            status: "REDIS_URL not set (skipped)".to_string(),
            ok: true,
        };
    }

    let host_port = if let Some(stripped) = trimmed.strip_prefix("redis://") {
        stripped
    } else if let Some(stripped) = trimmed.strip_prefix("rediss://") {
        stripped
    } else {
        trimmed
    };

    let without_auth = if let Some(idx) = host_port.find('@') {
        &host_port[idx + 1..]
    } else {
        host_port
    };

    let without_path = if let Some(idx) = without_auth.find('/') {
        &without_auth[..idx]
    } else {
        without_auth
    };

    let (host, port) = if let Some(idx) = without_path.find(':') {
        (&without_path[..idx], without_path[idx + 1..].parse::<u16>().unwrap_or(6379))
    } else {
        (without_path, 6379)
    };

    use std::net::ToSocketAddrs;
    let addrs = match format!("{host}:{port}").to_socket_addrs() {
        Ok(addrs) => addrs.collect::<Vec<_>>(),
        Err(e) => {
            return DoctorCheck {
                label: "redis connectivity".to_string(),
                status: format!("failed to resolve Redis host `{host}`: {e}"),
                ok: false,
            };
        }
    };

    for addr in addrs {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).is_ok() {
            return DoctorCheck {
                label: "redis connectivity".to_string(),
                status: format!("connected to Redis at {host}:{port}"),
                ok: true,
            };
        }
    }

    DoctorCheck {
        label: "redis connectivity".to_string(),
        status: format!("failed to connect to Redis at {host}:{port}"),
        ok: false,
    }
}

fn check_module_dag() -> (String, bool) {
    let src_dir = Path::new("src");
    if !src_dir.exists() {
        return ("src directory not found (skipped)".to_string(), true);
    }

    let mut files = Vec::new();
    fn visit_dirs(dir: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit_dirs(&path, files);
                } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                    files.push(path);
                }
            }
        }
    }
    visit_dirs(src_dir, &mut files);

    let mut deps_map: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for file_path in files {
        let content_opt = std::fs::read_to_string(&file_path).ok();
        if let Some(name) = content_opt.as_deref().and_then(extract_module_name) {
            let deps = content_opt
                .as_deref()
                .map(extract_module_dependencies)
                .unwrap_or_default();
            deps_map.insert(name, deps);
        }
    }

    if deps_map.is_empty() {
        return ("no module declarations found in src/ (acyclic)".to_string(), true);
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum VisitState {
        Unvisited,
        Visiting,
        Visited,
    }
    let mut states: BTreeMap<String, VisitState> = deps_map
        .keys()
        .map(|k| (k.clone(), VisitState::Unvisited))
        .collect();
    let mut path: Vec<String> = Vec::new();

    fn dfs(
        node: &str,
        deps_map: &BTreeMap<String, Vec<String>>,
        states: &mut BTreeMap<String, VisitState>,
        path: &mut Vec<String>,
    ) -> Result<(), Vec<String>> {
        states.insert(node.to_string(), VisitState::Visiting);
        path.push(node.to_string());

        if let Some(deps) = deps_map.get(node) {
            for dep in deps {
                match states.get(dep).copied().unwrap_or(VisitState::Unvisited) {
                    VisitState::Visiting => {
                        let start = path.iter().position(|n| n == dep).unwrap_or(0);
                        let mut cycle = path[start..].to_vec();
                        cycle.push(dep.clone());
                        return Err(cycle);
                    }
                    VisitState::Unvisited => {
                        dfs(dep, deps_map, states, path)?;
                    }
                    VisitState::Visited => {}
                }
            }
        }

        path.pop();
        states.insert(node.to_string(), VisitState::Visited);
        Ok(())
    }

    for node in deps_map.keys() {
        if states.get(node) == Some(&VisitState::Unvisited)
            && let Err(cycle) = dfs(node, &deps_map, &mut states, &mut path)
        {
            return (
                format!("dependency cycle detected: {}", cycle.join(" -> ")),
                false,
            );
        }
    }

    let count = deps_map.len();
    (format!("{count} module(s) verified acyclic"), true)
}

fn extract_module_name(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        let is_name_def = (trimmed.contains("fn name(") || trimmed.contains("name(&self)"))
            && trimmed.contains('"');
        if is_name_def {
            trimmed
                .split('"')
                .nth(1)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        } else {
            None
        }
    })
}

fn extract_module_dependencies(content: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_deps_fn = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.contains("fn dependencies(") || trimmed.contains("dependencies(&self)") {
            in_deps_fn = true;
        }
        if in_deps_fn {
            let mut remaining = trimmed;
            while let Some(start) = remaining.find('"') {
                if let Some(end) = remaining[start + 1..].find('"') {
                    deps.push(remaining[start + 1..start + 1 + end].to_string());
                    remaining = &remaining[start + 1 + end + 1..];
                } else {
                    break;
                }
            }
            if trimmed.contains('}') {
                break;
            }
        }
    }
    deps
}
