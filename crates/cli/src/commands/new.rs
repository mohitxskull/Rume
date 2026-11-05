use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum NewError {
    InvalidName(String),
    AlreadyExists(String),
    Io(std::io::Error),
}

impl std::fmt::Display for NewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(name) => {
                write!(
                    f,
                    "project name must contain only letters, numbers, '-' or '_': {name}"
                )
            }
            Self::AlreadyExists(name) => write!(f, "directory '{name}' already exists"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for NewError {}

pub fn resolve_rume_dependency(
    path_override: Option<&str>,
    version_override: Option<&str>,
) -> (String, Option<String>) {
    let version = version_override.unwrap_or(env!("CARGO_PKG_VERSION"));
    let path = path_override
        .map(String::from)
        .or_else(|| std::env::var("RUME_DEV_PATH").ok())
        .or_else(|| std::env::var("RUME_FRAMEWORK_PATH").ok());

    match path {
        Some(p) => {
            let dep = format!("rume = {{ version = \"{version}\", path = \"{p}\" }}");
            (dep, Some(p))
        }
        None => {
            let dep = format!("rume = \"{version}\"");
            (dep, None)
        }
    }
}

struct TemplateContext<'a> {
    name: &'a str,
    rume_dependency: &'a str,
}

const TEMPLATES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../../templates/Cargo.toml.tpl")),
    ("src/lib.rs", include_str!("../../templates/src/lib.rs.tpl")),
    (
        "src/main.rs",
        include_str!("../../templates/src/main.rs.tpl"),
    ),
    ("src/app.rs", include_str!("../../templates/src/app.rs.tpl")),
    ("src/env.rs", include_str!("../../templates/src/env.rs.tpl")),
    (
        "src/config.rs",
        include_str!("../../templates/src/config.rs.tpl"),
    ),
    (
        "src/routes.rs",
        include_str!("../../templates/src/routes.rs.tpl"),
    ),
    (
        "src/handlers/mod.rs",
        include_str!("../../templates/src/handlers/mod.rs.tpl"),
    ),
    (
        "src/handlers/health.rs",
        include_str!("../../templates/src/handlers/health.rs.tpl"),
    ),
    (
        "src/migrations.rs",
        include_str!("../../templates/src/migrations.rs.tpl"),
    ),
    (
        "src/queries.rs",
        include_str!("../../templates/src/queries.rs.tpl"),
    ),
    (
        "src/repository.rs",
        include_str!("../../templates/src/repository.rs.tpl"),
    ),
    (
        "sql/items/create.sql",
        include_str!("../../templates/sql/items/create.sql.tpl"),
    ),
    (
        "sql/items/find.sql",
        include_str!("../../templates/sql/items/find.sql.tpl"),
    ),
    (
        "sql/items/list.sql",
        include_str!("../../templates/sql/items/list.sql.tpl"),
    ),
    (
        "migrations/00000000000000_init.up.sql",
        include_str!("../../templates/migrations/00000000000000_init.up.sql.tpl"),
    ),
    (
        "migrations/00000000000000_init.down.sql",
        include_str!("../../templates/migrations/00000000000000_init.down.sql.tpl"),
    ),
    (
        "tests/smoke.rs",
        include_str!("../../templates/tests/smoke.rs.tpl"),
    ),
    (".env", include_str!("../../templates/.env.tpl")),
    (
        ".env.example",
        include_str!("../../templates/.env.example.tpl"),
    ),
    (
        "config/app.toml",
        include_str!("../../templates/config/app.toml.tpl"),
    ),
    ("README.md", include_str!("../../templates/README.md.tpl")),
];

pub fn run(name: &str, path_override: Option<&str>, version_override: Option<&str>) {
    let (rume_dep, resolved_path) = resolve_rume_dependency(path_override, version_override);

    if let Err(err) = run_inner(name, &rume_dep) {
        eprintln!("error: {err}");
        // `run_inner` already removed any directory this command created.
        // An `AlreadyExists` error means the directory pre-existed and must
        // never be deleted here.
        std::process::exit(1);
    }

    use crate::ui::sticker::{Instructions, Sticker};

    let mut sticker = Sticker::new();
    sticker.add(&format!("Created project `{}`", name));
    sticker.render();
    println!();

    let mut instructions = Instructions::new();
    instructions.add(&format!("cd {}", name));
    instructions.add("cp .env.example .env   # then edit .env and config/app.toml");
    instructions.add("cargo run               # run the app (validates env/config at boot)");
    instructions.add("cargo watch -x run      # or auto-reload during development");
    instructions.add("cargo test              # run tests (uses test env automatically)");
    instructions.add("");
    instructions.add("rume doctor            # validate project setup");
    instructions.add("rume --help            # see all commands");
    instructions.render();

    println!();
    match resolved_path {
        Some(path) => {
            eprintln!(
                "note: this project depends on rume via a local path ({path}). \
                 Before publishing or depending on crates.io, update Cargo.toml to pin \
                 a released rume version."
            );
        }
        None => {
            let version = version_override.unwrap_or(env!("CARGO_PKG_VERSION"));
            eprintln!("note: this project depends on published rume version {version}.");
        }
    }
}

fn run_inner(name: &str, rume_dependency: &str) -> Result<(), NewError> {
    validate_name(name)?;

    let root = Path::new(name);
    let already_exists = root.exists();
    if already_exists {
        // The directory pre-existed this command; never delete it.
        return Err(NewError::AlreadyExists(name.to_string()));
    }

    // `root` did not exist before we started, so anything we create below
    // is owned by this command and ours to clean up on failure.
    let created_root = !already_exists;

    let generate_result = (|| -> Result<(), NewError> {
        fs::create_dir_all(root.join("src")).map_err(NewError::Io)?;
        fs::create_dir_all(root.join("config")).map_err(NewError::Io)?;

        let context = TemplateContext {
            name,
            rume_dependency,
        };

        for (relative_path, template) in TEMPLATES {
            let rendered = render(template, &context);
            write_file(&root.join(relative_path), &rendered)?;
        }

        Ok(())
    })();

    if let Err(err) = generate_result {
        if created_root {
            // Only remove directories this command created. Never touch a
            // directory that existed before the command started.
            let _ = fs::remove_dir_all(root);
        }
        return Err(err);
    }

    Ok(())
}

fn validate_name(name: &str) -> Result<(), NewError> {
    if name.is_empty() {
        return Err(NewError::InvalidName(name.to_string()));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(NewError::InvalidName(format!(
            "project name `{name}` must start with an ASCII letter or underscore"
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(NewError::InvalidName(name.to_string()));
    }
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    const RESERVED_KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ];
    if RESERVED_KEYWORDS.contains(&normalized.as_str()) {
        return Err(NewError::InvalidName(format!(
            "project name `{name}` is a reserved Rust keyword"
        )));
    }
    Ok(())
}

fn render(template: &str, context: &TemplateContext<'_>) -> String {
    let crate_name = context.name.replace('-', "_");
    template
        .replace("{{name}}", context.name)
        .replace("{{crate_name}}", &crate_name)
        .replace("{{rume_dependency}}", context.rume_dependency)
}

fn write_file(path: &Path, content: &str) -> Result<(), NewError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(NewError::Io)?;
    }
    fs::write(path, content).map_err(NewError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        assert!(validate_name("demo").is_ok());
        assert!(validate_name("demo-app").is_ok());
        assert!(validate_name("demo_app").is_ok());
    }

    #[test]
    fn rejects_invalid_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("demo app").is_err());
        assert!(validate_name("demo/app").is_err());
        assert!(validate_name("2fa").is_err());
        assert!(validate_name("404").is_err());
        assert!(validate_name("type").is_err());
        assert!(validate_name("match").is_err());
        assert!(validate_name("-app").is_err());
    }

    #[test]
    fn resolves_portable_dependency_by_default() {
        let (dep, path) = resolve_rume_dependency(None, Some("0.1.0"));
        assert_eq!(dep, "rume = \"0.1.0\"");
        assert_eq!(path, None);
    }

    #[test]
    fn resolves_path_override_when_provided() {
        let (dep, path) = resolve_rume_dependency(Some("../crates/core"), Some("0.1.0"));
        assert_eq!(
            dep,
            "rume = { version = \"0.1.0\", path = \"../crates/core\" }"
        );
        assert_eq!(path, Some("../crates/core".to_string()));
    }

    #[test]
    fn renders_placeholders() {
        let context = TemplateContext {
            name: "demo",
            rume_dependency: "rume = \"0.1.0\"",
        };
        let rendered = render("name={{name}} dep={{rume_dependency}}", &context);
        assert_eq!(rendered, "name=demo dep=rume = \"0.1.0\"");
    }

    #[test]
    fn run_inner_already_exists_does_not_delete() {
        let name = format!("rume-new-test-{}", std::process::id());
        let root = Path::new(&name);
        fs::create_dir_all(root).expect("create dummy dir");
        fs::write(root.join("keep.txt"), "original contents").expect("write dummy file");

        let err = run_inner(&name, "rume = \"0.1.0\"").unwrap_err();
        assert!(matches!(err, NewError::AlreadyExists(_)));

        // The directory and its original contents must still exist.
        assert!(root.exists());
        assert_eq!(
            fs::read_to_string(root.join("keep.txt")).expect("read dummy file"),
            "original contents"
        );

        // Clean up the dummy dir ourselves; run_inner must not have touched it.
        let _ = fs::remove_dir_all(root);
    }
}
