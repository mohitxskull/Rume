use std::fs;

const TEMPLATE: &str = include_str!("../../../templates/src/model.rs.tpl");

pub fn run(name: &str) {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        eprintln!("error: model name must contain only letters, numbers, '-' or '_'");
        std::process::exit(1);
    }

    let module_name = name.to_ascii_lowercase().replace('-', "_");
    let struct_name = capitalize(&module_name);

    let dir = "src/models";
    let _ = std::fs::create_dir_all(dir);
    let model_path = format!("{dir}/{module_name}.rs");
    if std::path::Path::new(&model_path).exists() {
        eprintln!("error: model `{module_name}` already exists");
        std::process::exit(1);
    }

    let content = TEMPLATE.replace("{{Name}}", &struct_name);
    if let Err(err) = fs::write(&model_path, content) {
        eprintln!("error: failed to create {model_path}: {err}");
        std::process::exit(1);
    }

    let mod_path = format!("{dir}/mod.rs");
    let mod_entry = format!("pub mod {module_name};\n");
    match fs::read_to_string(&mod_path) {
        Ok(existing) if !existing.contains(&mod_entry) => {
            if let Err(err) = fs::write(&mod_path, format!("{existing}{mod_entry}")) {
                eprintln!("error: failed to update {mod_path}: {err}");
                std::process::exit(1);
            }
        }
        Ok(_) => {}
        Err(_) => {
            if let Err(err) = fs::write(&mod_path, mod_entry.clone()) {
                eprintln!("error: failed to create {mod_path}: {err}");
                std::process::exit(1);
            }
        }
    }

    println!("Generated model `{name}`");
    println!("  created {model_path}");
    println!("  updated {mod_path}");
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}