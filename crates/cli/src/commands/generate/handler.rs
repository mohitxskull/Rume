use std::fs;

pub fn run(name: &str) {
    super::validate_identifier(name, "handler");

    let module_name = name.to_ascii_lowercase().replace('-', "_");
    let struct_name = rume::helpers::pascal_case(&module_name);

    let handler_path = format!("src/handlers/{module_name}.rs");
    if std::path::Path::new(&handler_path).exists() {
        eprintln!("error: handler `{module_name}` already exists");
        std::process::exit(1);
    }

    // Pre-flight check: ensure routes.rs is present and has the marker BEFORE creating files
    let routes_path = "src/routes.rs";
    let route_line =
        format!("    router = router.get(\"/{name}\", crate::handlers::{module_name}::index);\n");
    let routes_content = match fs::read_to_string(routes_path) {
        Ok(existing) => {
            if !existing.contains(&route_line) && !existing.contains("    // rume:routes") {
                eprintln!(
                    "error: could not find route marker `// rume:routes` in {routes_path}; \
                     add the marker or register the route manually"
                );
                std::process::exit(1);
            }
            Some(existing)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            eprintln!("error: failed to read {routes_path}: {err}");
            std::process::exit(1);
        }
    };

    let content = format!(
        r#"use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct {struct_name}Response {{
    pub message: &'static str,
}}

pub async fn index() -> Json<{struct_name}Response> {{
    Json({struct_name}Response {{ message: "ok" }})
}}
"#
    );

    if let Err(err) = fs::write(&handler_path, content) {
        eprintln!("error: failed to create {handler_path}: {err}");
        std::process::exit(1);
    }

    // Register the module with cleanup on failure
    let mod_path = "src/handlers/mod.rs";
    let mod_entry = format!("pub mod {module_name};\n");
    let prev_mod_content = match fs::read_to_string(mod_path) {
        Ok(existing) => {
            if !existing.contains(&mod_entry)
                && let Err(err) = fs::write(mod_path, format!("{existing}{mod_entry}"))
            {
                let _ = fs::remove_file(&handler_path);
                eprintln!("error: failed to update {mod_path}: {err}");
                std::process::exit(1);
            }
            Some(existing)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Err(err) = fs::write(mod_path, &mod_entry) {
                let _ = fs::remove_file(&handler_path);
                eprintln!("error: failed to create {mod_path}: {err}");
                std::process::exit(1);
            }
            None
        }
        Err(err) => {
            let _ = fs::remove_file(&handler_path);
            eprintln!("error: failed to read {mod_path}: {err}");
            std::process::exit(1);
        }
    };

    // Register the route with cleanup on failure
    if let Some(existing) = routes_content
        && !existing.contains(&route_line)
        && let Some(marker) = existing.find("    // rume:routes")
    {
        let mut updated = String::with_capacity(existing.len() + route_line.len());
        updated.push_str(&existing[..marker]);
        updated.push_str(&route_line);
        updated.push_str(&existing[marker..]);
        if let Err(err) = fs::write(routes_path, updated) {
            let _ = fs::remove_file(&handler_path);
            if let Some(prev) = prev_mod_content {
                let _ = fs::write(mod_path, prev);
            }
            eprintln!("error: failed to update {routes_path}: {err}");
            std::process::exit(1);
        }
    }

    println!("Generated handler `{name}`");
    println!("  created {handler_path}");
    println!("  updated {mod_path}");
    println!("  updated {routes_path}");
}
