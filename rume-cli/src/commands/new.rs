use std::fs;
use std::path::Path;

// Resolved at compile time so generated projects build against this checkout
// during pre-v1 development. Switch to a crates.io version before publishing.
const FRAMEWORK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rume");

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

struct TemplateContext<'a> {
    name: &'a str,
    framework_path: &'a str,
}

const TEMPLATES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../../templates/Cargo.toml.tpl")),
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

pub fn run(name: &str) {
    if let Err(err) = run_inner(name) {
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
    instructions.render();

    // D-m3: warn about the local-path rume dependency.
    // Pre-v1, new projects scaffold against this checkout via a path dep.
    // Users must switch to a crates.io pin before publishing.
    println!();
    eprintln!(
        "note: this project depends on rume via a local path ({}). \
         Before publishing or depending on crates.io, update Cargo.toml to pin \
         a released rume version.",
        FRAMEWORK_PATH
    );
}

fn run_inner(name: &str) -> Result<(), NewError> {
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
            framework_path: FRAMEWORK_PATH,
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
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(NewError::InvalidName(name.to_string()));
    }
    Ok(())
}

fn render(template: &str, context: &TemplateContext<'_>) -> String {
    template
        .replace("{{name}}", context.name)
        .replace("{{framework_path}}", context.framework_path)
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
    }

    #[test]
    fn renders_placeholders() {
        let context = TemplateContext {
            name: "demo",
            framework_path: "/tmp/rume",
        };
        let rendered = render("name={{name}} path={{framework_path}}", &context);
        assert_eq!(rendered, "name=demo path=/tmp/rume");
    }

    #[test]
    fn run_inner_already_exists_does_not_delete() {
        let name = format!("rume-new-test-{}", std::process::id());
        let root = Path::new(&name);
        fs::create_dir_all(root).expect("create dummy dir");
        fs::write(root.join("keep.txt"), "original contents").expect("write dummy file");

        let err = run_inner(&name).unwrap_err();
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
