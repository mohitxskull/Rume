mod handler;
mod middleware;
mod model;
mod resource;
mod service;

pub(crate) fn is_valid_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return false;
    }
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    const RESERVED_KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ];
    !RESERVED_KEYWORDS.contains(&normalized.as_str())
}

pub(crate) fn validate_identifier(name: &str, entity_type: &str) {
    if name.is_empty() {
        eprintln!("error: {entity_type} name cannot be empty");
        std::process::exit(1);
    }
    if !is_valid_identifier(name) {
        eprintln!(
            "error: {entity_type} name `{name}` must be a valid Rust identifier and not a reserved keyword"
        );
        std::process::exit(1);
    }
}

pub fn run(kind: Option<&str>, name: Option<&str>) {
    let Some(kind) = kind else {
        println!("Available generators:");
        println!("  handler <name>     Generate an HTTP handler and register its route");
        println!("  service <name>     Generate a service struct");
        println!("  model <name>       Generate a model struct");
        println!("  middleware <name>  Generate a middleware function");
        println!("  resource <name>    Generate RESTful resource handlers and routes");
        println!();
        println!("Usage:");
        println!("  rume generate handler users");
        println!("  rume generate service users");
        println!("  rume generate model user");
        println!("  rume generate middleware auth");
        println!("  rume generate resource users");
        return;
    };

    let name = match name {
        Some(n) => n.to_string(),
        None => {
            let prompted = crate::prompts::ask(&format!("{} name", kind));
            if prompted.is_empty() {
                eprintln!("error: name is required");
                std::process::exit(1);
            }
            prompted
        }
    };

    match kind {
        "handler" => {
            handler::run(&name);
        }
        "service" => {
            service::run(&name);
        }
        "model" => {
            model::run(&name);
        }
        "middleware" => {
            middleware::run(&name);
        }
        "resource" => {
            resource::run(&name);
        }
        _ => {
            eprintln!("error: unknown generator `{kind}`");
            eprintln!("available generators: handler, service, model, middleware, resource");
            std::process::exit(1);
        }
    }
}

pub(crate) fn ensure_main_mod(module_name: &str) {
    let (target_path, is_pub) = if std::path::Path::new("src/lib.rs").is_file() {
        ("src/lib.rs", true)
    } else {
        ("src/main.rs", false)
    };
    if let Ok(content) = std::fs::read_to_string(target_path) {
        let mod_stmt = if is_pub {
            format!("pub mod {module_name};")
        } else {
            format!("mod {module_name};")
        };
        if !content.lines().any(|l| l.trim() == mod_stmt) {
            let lines: Vec<&str> = content.lines().collect();
            let last_mod_pos = lines.iter().rposition(|l| {
                let trimmed = l.trim();
                (trimmed.starts_with("mod ") || trimmed.starts_with("pub mod "))
                    && trimmed.ends_with(';')
            });
            let new_content = match last_mod_pos {
                Some(pos) => {
                    let mut res = String::new();
                    for (i, line) in lines.iter().enumerate() {
                        res.push_str(line);
                        res.push('\n');
                        if i == pos {
                            res.push_str(&mod_stmt);
                            res.push('\n');
                        }
                    }
                    res
                }
                None => format!("{mod_stmt}\n{content}"),
            };
            let _ = std::fs::write(target_path, new_content);
        }
    }
}
