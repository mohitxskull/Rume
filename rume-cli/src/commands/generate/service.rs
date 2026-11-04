use std::fs;

const TEMPLATE: &str = include_str!("../../../templates/src/service.rs.tpl");

pub fn run(name: &str) {
    super::validate_identifier(name, "service");

    let module_name = name.to_ascii_lowercase().replace('-', "_");
    let struct_name = rume::helpers::pascal_case(&module_name);

    let dir = "src/services";
    let _ = std::fs::create_dir_all(dir);
    let service_path = format!("{dir}/{module_name}.rs");
    if std::path::Path::new(&service_path).exists() {
        eprintln!("error: service `{module_name}` already exists");
        std::process::exit(1);
    }

    let content = TEMPLATE.replace("{{Name}}", &struct_name);
    if let Err(err) = fs::write(&service_path, content) {
        eprintln!("error: failed to create {service_path}: {err}");
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

    super::ensure_main_mod("services");

    println!("Generated service `{name}`");
    println!("  created {service_path}");
    println!("  updated {mod_path}");
}
