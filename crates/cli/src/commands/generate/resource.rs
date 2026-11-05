use std::fs;

pub fn run(name: &str) {
    super::validate_identifier(name, "resource");

    let module_name = name.to_ascii_lowercase().replace('-', "_");
    let struct_name = rume::helpers::pascal_case(&module_name);

    let handler_path = format!("src/handlers/{module_name}.rs");
    if std::path::Path::new(&handler_path).exists() {
        eprintln!("error: handler `{module_name}` already exists");
        std::process::exit(1);
    }

    // Pre-flight check: ensure routes.rs is present and has the marker BEFORE creating files
    let routes_path = "src/routes.rs";
    let route_lines = [
        format!(
            "    router = router.get(\"/{name}\", crate::handlers::{module_name}::index);\n",
            name = name,
            module_name = module_name
        ),
        format!(
            "    router = router.get(\"/{name}/{{id}}\", crate::handlers::{module_name}::show);\n",
            name = name,
            module_name = module_name
        ),
        format!(
            "    router = router.post(\"/{name}\", crate::handlers::{module_name}::create);\n",
            name = name,
            module_name = module_name
        ),
        format!(
            "    router = router.put(\"/{name}/{{id}}\", crate::handlers::{module_name}::update);\n",
            name = name,
            module_name = module_name
        ),
        format!(
            "    router = router.delete(\"/{name}/{{id}}\", crate::handlers::{module_name}::destroy);\n",
            name = name,
            module_name = module_name
        ),
    ];
    let routes_len: usize = route_lines.iter().map(|line| line.len()).sum();
    let routes_content = match fs::read_to_string(routes_path) {
        Ok(existing) => {
            if !existing.contains(&route_lines[0]) && !existing.contains("    // rume:routes") {
                eprintln!(
                    "error: could not find route marker `// rume:routes` in {routes_path}; \
                     add the marker or register the routes manually"
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
use axum::extract::Path;
use serde::{{Deserialize, Serialize}};

#[derive(Debug, Deserialize)]
pub struct {struct_name}CreateRequest {{
    pub message: Option<String>,
}}

#[derive(Debug, Deserialize)]
pub struct {struct_name}UpdateRequest {{
    pub message: Option<String>,
}}

#[derive(Serialize)]
pub struct {struct_name}IndexResponse {{
    pub message: &'static str,
}}

#[derive(Serialize)]
pub struct {struct_name}ShowResponse {{
    pub message: String,
}}

#[derive(Serialize)]
pub struct {struct_name}CreateResponse {{
    pub message: &'static str,
}}

#[derive(Serialize)]
pub struct {struct_name}UpdateResponse {{
    pub message: String,
}}

#[derive(Serialize)]
pub struct {struct_name}DestroyResponse {{
    pub message: String,
}}

pub async fn index() -> Json<{struct_name}IndexResponse> {{
    Json({struct_name}IndexResponse {{ message: "list" }})
}}

pub async fn show(Path(id): Path<String>) -> Json<{struct_name}ShowResponse> {{
    Json({struct_name}ShowResponse {{
        message: format!("show {{id}}")
    }})
}}

pub async fn create(
    Json(_payload): Json<{struct_name}CreateRequest>,
) -> Json<{struct_name}CreateResponse> {{
    Json({struct_name}CreateResponse {{ message: "created" }})
}}

pub async fn update(
    Path(id): Path<String>,
    Json(_payload): Json<{struct_name}UpdateRequest>,
) -> Json<{struct_name}UpdateResponse> {{
    Json({struct_name}UpdateResponse {{
        message: format!("update {{id}}")
    }})
}}

pub async fn destroy(Path(id): Path<String>) -> Json<{struct_name}DestroyResponse> {{
    Json({struct_name}DestroyResponse {{
        message: format!("delete {{id}}")
    }})
}}
"#,
        struct_name = struct_name
    );

    if let Err(err) = fs::write(&handler_path, content) {
        eprintln!("error: failed to create {handler_path}: {err}");
        std::process::exit(1);
    }

    // Register the module with rollback on error.
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

    // Register the routes with rollback on error.
    if let Some(existing) = routes_content
        && !existing.contains(&route_lines[0])
        && let Some(marker) = existing.find("    // rume:routes")
    {
        let mut updated = String::with_capacity(existing.len() + routes_len);
        updated.push_str(&existing[..marker]);
        for line in &route_lines {
            updated.push_str(line);
        }
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

    println!("Generated resource `{name}`");
    println!("  created {handler_path}");
    println!("  updated {mod_path}");
    println!("  updated {routes_path}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn validate_name_rejects_empty_and_invalid() {
        assert!(!super::super::is_valid_identifier(""));
        assert!(!super::super::is_valid_identifier("bad name"));
        assert!(!super::super::is_valid_identifier("bad/name"));
        assert!(!super::super::is_valid_identifier("bad.name"));
        assert!(!super::super::is_valid_identifier("2fa"));
        assert!(!super::super::is_valid_identifier("404"));
        assert!(!super::super::is_valid_identifier("type"));
        assert!(!super::super::is_valid_identifier("match"));
    }

    #[test]
    fn validate_name_accepts_valid() {
        assert!(super::super::is_valid_identifier("users"));
        assert!(super::super::is_valid_identifier("my-resource"));
        assert!(super::super::is_valid_identifier("my_resource"));
        assert!(super::super::is_valid_identifier("User123"));
    }

    #[test]
    fn module_name_and_struct_name_derivation() {
        let cases = [
            ("users", "users", "Users"),
            ("my-resource", "my_resource", "MyResource"),
            ("user_profile", "user_profile", "UserProfile"),
            ("User123", "user123", "User123"),
        ];
        for (input, expected_module, expected_struct) in cases {
            let module_name = input.to_ascii_lowercase().replace('-', "_");
            let struct_name = rume::helpers::pascal_case(&module_name);
            assert_eq!(module_name, expected_module);
            assert_eq!(struct_name, expected_struct);
        }
    }
}
