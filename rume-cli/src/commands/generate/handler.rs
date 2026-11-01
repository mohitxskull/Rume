use std::fs;

pub fn run(name: &str) {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        eprintln!("error: handler name must contain only letters, numbers, '-' or '_'");
        std::process::exit(1);
    }

    let module_name = name.to_ascii_lowercase().replace('-', "_");
    let struct_name = capitalize(&module_name);

    let handler_path = format!("src/handlers/{module_name}.rs");
    if std::path::Path::new(&handler_path).exists() {
        eprintln!("error: handler `{module_name}` already exists");
        std::process::exit(1);
    }

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
"#,
        struct_name = struct_name
    );

    if let Err(err) = fs::write(&handler_path, content) {
        eprintln!("error: failed to create {handler_path}: {err}");
        std::process::exit(1);
    }

    // Register the module.
    let mod_path = "src/handlers/mod.rs";
    let mod_entry = format!("pub mod {module_name};\n");
    match fs::read_to_string(mod_path) {
        Ok(existing) if !existing.contains(&mod_entry) => {
            if let Err(err) = fs::write(mod_path, format!("{existing}{mod_entry}")) {
                eprintln!("error: failed to update {mod_path}: {err}");
                std::process::exit(1);
            }
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("error: failed to read {mod_path}: {err}");
            std::process::exit(1);
        }
    }

    // Register the route.
    let routes_path = "src/routes.rs";
    match fs::read_to_string(routes_path) {
        Ok(existing) => {
            let route_line = format!(
                "    router = router.route(\"/{name}\", axum::routing::get(crate::handlers::{module_name}::index));\n",
                name = name,
                module_name = module_name
            );
            if existing.contains(&route_line) {
                // Already registered.
            } else if let Some(marker) = existing.find("    // rume:routes") {
                let mut updated = String::with_capacity(existing.len() + route_line.len());
                updated.push_str(&existing[..marker]);
                updated.push_str(&route_line);
                updated.push_str(&existing[marker..]);
                if let Err(err) = fs::write(routes_path, updated) {
                    eprintln!("error: failed to update {routes_path}: {err}");
                    std::process::exit(1);
                }
            } else {
                eprintln!(
                    "error: could not find route marker `// rume:routes` in {routes_path}; \
                     add the marker or register the route manually"
                );
                std::process::exit(1);
            }
        }
        Err(err) => {
            eprintln!("error: failed to read {routes_path}: {err}");
            std::process::exit(1);
        }
    }

    println!("Generated handler `{name}`");
    println!("  created {handler_path}");
    println!("  updated {mod_path}");
    println!("  updated {routes_path}");
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}
