//! Database migration generation, status inspection, and execution CLI commands.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use comfy_table::Color;
use console::style;
use rume::persistence::config::DatabaseConfig;
use rume::persistence::migration::{Migration, Migrator};
use rume::persistence::{MysqlBackend, PostgresBackend, SqliteBackend};

use crate::cli::DbCommands;
use crate::ui::table::TableBuilder;

pub async fn run(cmd: DbCommands) {
    match cmd {
        DbCommands::Status => status().await,
        DbCommands::Migrate => migrate().await,
        DbCommands::Rollback => rollback().await,
    }
}

pub fn make_migration(name: Option<String>) {
    let raw_name = match name {
        Some(n) if !n.trim().is_empty() => n,
        _ => crate::prompts::ask("migration name"),
    };

    let raw_name = raw_name.trim();
    if raw_name.is_empty() {
        eprintln!("{}", style("error: migration name is required").red());
        std::process::exit(1);
    }

    let clean_name: String = raw_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let clean_name = clean_name.trim_matches('_');
    if clean_name.is_empty() {
        eprintln!(
            "{}",
            style("error: migration name must contain at least one ASCII alphanumeric character")
                .red()
        );
        std::process::exit(1);
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let migration_id = format!("{timestamp}_{clean_name}");

    let migrations_dir = Path::new("migrations");
    if let Err(e) = fs::create_dir_all(migrations_dir) {
        eprintln!(
            "{}",
            style(format!("error: failed to create migrations directory: {e}")).red()
        );
        std::process::exit(1);
    }

    let up_filename = format!("{migration_id}.up.sql");
    let down_filename = format!("{migration_id}.down.sql");

    let up_path = migrations_dir.join(&up_filename);
    let down_path = migrations_dir.join(&down_filename);

    let up_content =
        format!("-- Migration: {clean_name} (Up)\n-- Write your SQL migration statements here.\n");
    let down_content =
        format!("-- Migration: {clean_name} (Down)\n-- Write your SQL rollback statements here.\n");

    if let Err(e) = fs::write(&up_path, up_content) {
        eprintln!(
            "{}",
            style(format!("error: failed to write {}: {e}", up_path.display())).red()
        );
        std::process::exit(1);
    }
    if let Err(e) = fs::write(&down_path, down_content) {
        eprintln!(
            "{}",
            style(format!(
                "error: failed to write {}: {e}",
                down_path.display()
            ))
            .red()
        );
        std::process::exit(1);
    }

    println!(
        "{}",
        style(format!("✓ Created migrations/{}", up_filename))
            .green()
            .bold()
    );
    println!(
        "{}",
        style(format!("✓ Created migrations/{}", down_filename))
            .green()
            .bold()
    );
}

pub(crate) fn discover_local_migrations() -> Migrator {
    let mut migrator = Migrator::new();
    let migrations_dir = Path::new("migrations");
    if !migrations_dir.exists() {
        return migrator;
    }

    let mut up_files = BTreeMap::new();
    let mut down_files = BTreeSet::new();

    if let Ok(entries) = fs::read_dir(migrations_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(id) = file_name.strip_suffix(".up.sql") {
                if let Ok(content) = fs::read_to_string(&path) {
                    up_files.insert(id.to_string(), content);
                }
            } else if let Some(id) = file_name.strip_suffix(".down.sql") {
                down_files.insert(id.to_string());
            }
        }
    }

    let down_set: BTreeSet<String> = down_files;

    for (id, up_sql) in up_files {
        let desc = id.replace('_', " ");
        let mut m = Migration::from_sql(&id, desc, up_sql);
        if down_set.contains(&id) {
            let down_path = migrations_dir.join(format!("{id}.down.sql"));
            if let Ok(down_sql) = fs::read_to_string(down_path) {
                m = m.with_down(down_sql);
            }
        }
        migrator = migrator.add_migration(m);
    }

    migrator
}

#[derive(serde::Deserialize)]
struct DatabaseFileConfig {
    database: Option<DatabaseSection>,
}

#[derive(serde::Deserialize)]
struct DatabaseSection {
    url: Option<String>,
}

pub(crate) fn resolve_db_url() -> Result<Option<String>, String> {
    resolve_db_url_in(Path::new("."))
}

pub(crate) fn resolve_db_url_in(dir: &Path) -> Result<Option<String>, String> {
    let env_name = rume::env::resolve_env_name();
    resolve_db_url_for_env_in(dir, &env_name)
}

pub(crate) fn resolve_db_url_for_env_in(
    dir: &Path,
    env_name: &str,
) -> Result<Option<String>, String> {
    resolve_db_url_for_env_with_vars(dir, env_name, std::env::vars())
}

pub(crate) fn resolve_db_url_for_env_with_vars<I>(
    dir: &Path,
    env_name: &str,
    process_vars: I,
) -> Result<Option<String>, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    // Why: parse `.env` files raw (without `dotenvy`'s eager per-file `${VAR}`
    // expansion) and run `rume::env::interpolate_value` only after all `.env*`
    // layers and process env are merged, so overrides in `.env.{env}` /
    // `.env.local` drive `${VAR}` expansion inside `DATABASE_URL`.
    // Also fail closed on any `.env` load/parse/interpolation error instead of
    // silently falling back to `config/app*.toml`.
    let mut merged: BTreeMap<String, String> = BTreeMap::new();

    for path in dotenv_candidates(dir, env_name) {
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read environment file {}: {e}", path.display()))?;
        for (key, raw_value) in parse_dotenv(&content, &path)? {
            let value = resolve_file_value(raw_value, dir, &path)?;
            merged.insert(key, value);
        }
    }

    for (key, value) in process_vars {
        merged.insert(key, value);
    }

    if let Some(raw_url) = merged.get("DATABASE_URL") {
        let resolved = rume::env::interpolate_value(raw_url, &merged)
            .map_err(|e| format!("failed to resolve DATABASE_URL: {e}"))?;
        let trimmed = resolved.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }

    let Some(cfg) = rume::config::load_in::<DatabaseFileConfig>(dir).ok() else {
        return Ok(None);
    };
    let Some(url) = cfg.database.and_then(|d| d.url) else {
        return Ok(None);
    };
    let trimmed = url.trim();
    if !trimmed.is_empty() {
        Ok(Some(trimmed.to_string()))
    } else {
        Ok(None)
    }
}

fn dotenv_candidates(dir: &Path, env_name: &str) -> Vec<PathBuf> {
    let is_test = env_name == "test" || env_name == "testing";
    let mut candidates = vec![dir.join(".env"), dir.join(format!(".env.{env_name}"))];
    if !is_test {
        candidates.push(dir.join(".env.local"));
    }
    candidates.push(dir.join(format!(".env.{env_name}.local")));
    candidates
}

fn parse_dotenv(content: &str, path: &Path) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let after_export = trimmed
            .strip_prefix("export ")
            .or_else(|| trimmed.strip_prefix("export\t"))
            .map(str::trim_start)
            .unwrap_or(trimmed);

        let Some((raw_key, raw_val)) = after_export.split_once('=') else {
            return Err(format!(
                "failed to parse environment file {} at line {line_no}: missing '='",
                path.display()
            ));
        };

        let key = raw_key.trim();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            return Err(format!(
                "failed to parse environment file {} at line {line_no}: invalid variable name",
                path.display()
            ));
        }

        let value = parse_dotenv_value(raw_val, path, line_no)?;
        entries.push((key.to_string(), value));
    }
    Ok(entries)
}

fn parse_dotenv_value(raw_val: &str, path: &Path, line_no: usize) -> Result<String, String> {
    let val = raw_val.trim_start();
    if let Some(rest) = val.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        let mut closed = false;
        while let Some(ch) = chars.next() {
            match ch {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => break,
                },
                other => out.push(other),
            }
        }
        if !closed {
            return Err(format!(
                "failed to parse environment file {} at line {line_no}: unclosed double quote",
                path.display()
            ));
        }
        let trailing = chars.as_str().trim_start();
        if !trailing.is_empty() && !trailing.starts_with('#') {
            return Err(format!(
                "failed to parse environment file {} at line {line_no}: unexpected trailing characters",
                path.display()
            ));
        }
        Ok(out)
    } else if let Some(rest) = val.strip_prefix('\'') {
        let Some((inner, trailing)) = rest.split_once('\'') else {
            return Err(format!(
                "failed to parse environment file {} at line {line_no}: unclosed single quote",
                path.display()
            ));
        };
        let trailing = trailing.trim_start();
        if !trailing.is_empty() && !trailing.starts_with('#') {
            return Err(format!(
                "failed to parse environment file {} at line {line_no}: unexpected trailing characters",
                path.display()
            ));
        }
        Ok(inner.to_string())
    } else {
        // Strip inline `# comment` only when preceded by whitespace.
        let mut end = val.len();
        let mut prev_ws = false;
        for (i, ch) in val.char_indices() {
            if ch == '#' && prev_ws {
                end = i;
                break;
            }
            prev_ws = ch.is_whitespace();
        }
        Ok(val[..end].trim_end().to_string())
    }
}

fn resolve_file_value(value: String, dir: &Path, source_path: &Path) -> Result<String, String> {
    if let Some(rest) = value.strip_prefix("file\\:") {
        return Ok(format!("file:{rest}"));
    }
    let Some(rel_or_abs) = value.strip_prefix("file:") else {
        return Ok(value);
    };

    let full = if Path::new(rel_or_abs).is_absolute() {
        PathBuf::from(rel_or_abs)
    } else {
        dir.join(rel_or_abs)
    };
    // Why: fail closed on `fs::canonicalize` errors rather than falling back to raw paths, and
    // read `&canonical_full` instead of `&full` so dangling targets or symlink escapes cannot
    // bypass workspace containment.
    let canonical_dir = fs::canonicalize(dir).map_err(|e| {
        format!(
            "failed to canonicalize workspace directory for file: reference in {}: {e}",
            source_path.display()
        )
    })?;
    let canonical_full = fs::canonicalize(&full).map_err(|e| {
        format!(
            "failed to canonicalize file: reference in {}: {e}",
            source_path.display()
        )
    })?;
    if !canonical_full.starts_with(&canonical_dir) {
        return Err(format!(
            "failed to resolve file: reference in {}: path escapes workspace directory",
            source_path.display()
        ));
    }

    let contents = fs::read_to_string(&canonical_full).map_err(|e| {
        format!(
            "failed to read file: reference in {}: {e}",
            source_path.display()
        )
    })?;
    Ok(contents
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string())
}

pub(crate) enum BackendHandle {
    Postgres(PostgresBackend),
    Sqlite(SqliteBackend),
    Mysql(MysqlBackend),
}

pub(crate) async fn connect_backend(url: &str) -> Result<(BackendHandle, &'static str), String> {
    let config = DatabaseConfig::new(url);
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        let backend = PostgresBackend::new(&config)
            .map_err(|e| format!("failed to initialize PostgreSQL backend: {e}"))?;
        Ok((BackendHandle::Postgres(backend), "postgres"))
    } else if url.starts_with("mysql://") || url.starts_with("mariadb://") {
        // Why: MariaDB speaks the MySQL wire protocol and sqlx-mysql natively
        // accepts `mariadb://` URLs, so routing `mariadb://` to `MysqlBackend`
        // shares the MySQL pool/migrator without duplicating the backend.
        let backend = MysqlBackend::new(&config)
            .map_err(|e| format!("failed to initialize MySQL backend: {e}"))?;
        Ok((BackendHandle::Mysql(backend), "mysql"))
    } else {
        let backend = SqliteBackend::new(&config)
            .await
            .map_err(|e| format!("failed to initialize SQLite backend: {e}"))?;
        Ok((BackendHandle::Sqlite(backend), "sqlite"))
    }
}

pub async fn status() {
    let db_url = match resolve_db_url() {
        Ok(url) => url,
        Err(err) => {
            eprintln!("{}", style(format!("error: {err}")).red());
            std::process::exit(1);
        }
    };
    let local_migrator = discover_local_migrations();

    let mut builder = TableBuilder::new();
    builder.head(&["Migration ID", "Status", "Applied At"]);

    let conn_res = match &db_url {
        Some(url) => connect_backend(url).await.ok(),
        None => None,
    };

    let mut row_count = 0;

    if let Some((handle, dialect)) = conn_res {
        let migrator = local_migrator.with_rume_core(dialect);
        let applied_res = match &handle {
            BackendHandle::Postgres(b) => migrator.applied_migrations(b).await,
            BackendHandle::Sqlite(b) => migrator.applied_migrations(b).await,
            BackendHandle::Mysql(b) => migrator.applied_migrations(b).await,
        };

        match applied_res {
            Ok(applied_list) => {
                let applied_map: BTreeMap<String, i64> = applied_list.into_iter().collect();
                for m in migrator.migrations() {
                    row_count += 1;
                    match applied_map.get(&m.id) {
                        Some(ts) => {
                            let formatted_date = chrono::DateTime::from_timestamp(*ts, 0)
                                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                                .unwrap_or_else(|| ts.to_string());
                            builder.row_colored(&[
                                (&m.id, None),
                                ("Applied", Some(Color::Green)),
                                (&formatted_date, None),
                            ]);
                        }
                        None => {
                            builder.row_colored(&[
                                (&m.id, None),
                                ("Pending", Some(Color::Yellow)),
                                ("-", None),
                            ]);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    style(format!("warning: failed to query migration ledger: {e}")).yellow()
                );
                for m in migrator.migrations() {
                    row_count += 1;
                    builder.row_colored(&[
                        (&m.id, None),
                        ("Unknown", Some(Color::Yellow)),
                        ("-", None),
                    ]);
                }
            }
        }
    } else {
        if db_url.is_none() {
            println!(
                "{}",
                style("Note: DATABASE_URL not set (showing local migrations only)").dim()
            );
        } else {
            println!(
                "{}",
                style("Note: Database offline or unreachable (showing local migrations only)")
                    .dim()
            );
        }
        for m in local_migrator.migrations() {
            row_count += 1;
            builder.row_colored(&[(&m.id, None), ("Local", Some(Color::Blue)), ("-", None)]);
        }
    }

    if row_count == 0 {
        println!("No migrations found in migrations/ directory.");
    } else {
        builder.render();
    }
}

pub async fn migrate() {
    let db_url = match resolve_db_url() {
        Ok(Some(url)) => url,
        Ok(None) => {
            eprintln!("{}", style("error: DATABASE_URL is not set and database.url was not found in config/app.toml").red());
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("{}", style(format!("error: {err}")).red());
            std::process::exit(1);
        }
    };

    let (handle, dialect) = match connect_backend(&db_url).await {
        Ok(res) => res,
        Err(err) => {
            eprintln!("{}", style(err).red());
            std::process::exit(1);
        }
    };

    let migrator = discover_local_migrations().with_rume_core(dialect);

    let summary_res = match &handle {
        BackendHandle::Postgres(b) => migrator.run_pending(b).await,
        BackendHandle::Sqlite(b) => migrator.run_pending(b).await,
        BackendHandle::Mysql(b) => migrator.run_pending(b).await,
    };

    match summary_res {
        Ok(summary) => {
            if summary.applied.is_empty() {
                println!(
                    "{}",
                    style("All migrations are already up to date.").green()
                );
            } else {
                for id in &summary.applied {
                    println!("{}", style(format!("✓ Applied: {id}")).green().bold());
                }
                println!(
                    "{}",
                    style(format!(
                        "Successfully applied {} migration(s).",
                        summary.applied.len()
                    ))
                    .green()
                    .bold()
                );
            }
        }
        Err(e) => {
            eprintln!("{}", style(format!("error: migration failed: {e}")).red());
            std::process::exit(1);
        }
    }
}

pub async fn rollback() {
    let db_url = match resolve_db_url() {
        Ok(Some(url)) => url,
        Ok(None) => {
            eprintln!("{}", style("error: DATABASE_URL is not set and database.url was not found in config/app.toml").red());
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("{}", style(format!("error: {err}")).red());
            std::process::exit(1);
        }
    };

    let (handle, dialect) = match connect_backend(&db_url).await {
        Ok(res) => res,
        Err(err) => {
            eprintln!("{}", style(err).red());
            std::process::exit(1);
        }
    };

    let migrator = discover_local_migrations().with_rume_core(dialect);

    let rollback_res = match &handle {
        BackendHandle::Postgres(b) => migrator.rollback_last(b).await,
        BackendHandle::Sqlite(b) => migrator.rollback_last(b).await,
        BackendHandle::Mysql(b) => migrator.rollback_last(b).await,
    };

    match rollback_res {
        Ok(Some(id)) => {
            println!("{}", style(format!("✓ Rolled back: {id}")).green().bold());
        }
        Ok(None) => {
            println!("{}", style("No migrations to roll back.").yellow());
        }
        Err(e) => {
            eprintln!("{}", style(format!("error: rollback failed: {e}")).red());
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackendHandle, connect_backend, resolve_db_url_for_env_in,
        resolve_db_url_for_env_with_vars, resolve_file_value,
    };
    use std::path::Path;

    #[tokio::test]
    async fn connect_backend_routes_mariadb_to_mysql() {
        let (handle, dialect) = connect_backend("mariadb://user:pass@localhost:3306/app")
            .await
            .expect("mariadb:// URL should initialize MySQL-compatible backend");
        assert_eq!(dialect, "mysql");
        assert!(matches!(handle, BackendHandle::Mysql(_)));
    }

    #[tokio::test]
    async fn connect_backend_routes_mysql_postgres_and_sqlite() {
        let (mysql_handle, mysql_dialect) = connect_backend("mysql://user:pass@localhost:3306/app")
            .await
            .expect("mysql:// URL should initialize MySQL backend");
        assert_eq!(mysql_dialect, "mysql");
        assert!(matches!(mysql_handle, BackendHandle::Mysql(_)));

        let (pg_handle, pg_dialect) = connect_backend("postgres://user:pass@localhost:5432/app")
            .await
            .expect("postgres:// URL should initialize PostgreSQL backend");
        assert_eq!(pg_dialect, "postgres");
        assert!(matches!(pg_handle, BackendHandle::Postgres(_)));

        let (sqlite_handle, sqlite_dialect) = connect_backend("sqlite::memory:")
            .await
            .expect("sqlite::memory: should initialize SQLite backend");
        assert_eq!(sqlite_dialect, "sqlite");
        assert!(matches!(sqlite_handle, BackendHandle::Sqlite(_)));
    }

    #[test]
    fn resolve_db_url_uses_env_interpolation_and_config_layering() {
        let dir = tempfile::tempdir().unwrap();
        let resolve_isolated =
            |path: &Path| resolve_db_url_for_env_with_vars(path, "development", std::iter::empty());

        // Neither .env nor config/app.toml present -> Ok(None)
        assert_eq!(resolve_isolated(dir.path()), Ok(None));

        // config/app.toml + config/app.local.toml override
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/app.toml"),
            "[database]\nurl = \"sqlite://base.db\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config/app.local.toml"),
            "[database]\nurl = \"mariadb://local:3306/app\"\n",
        )
        .unwrap();
        assert_eq!(
            resolve_isolated(dir.path()),
            Ok(Some("mariadb://local:3306/app".to_string()))
        );

        // Empty DATABASE_URL in .env falls through to config/app*.toml
        std::fs::write(dir.path().join(".env"), "DATABASE_URL=   \n").unwrap();
        assert_eq!(
            resolve_isolated(dir.path()),
            Ok(Some("mariadb://local:3306/app".to_string()))
        );

        // .env with ${VAR} interpolation takes precedence over config/app*.toml
        std::fs::write(
            dir.path().join(".env"),
            "DB_HOST=db.internal\nDATABASE_URL=postgres://app:secret@${DB_HOST}:5432/rume\n",
        )
        .unwrap();
        assert_eq!(
            resolve_isolated(dir.path()),
            Ok(Some(
                "postgres://app:secret@db.internal:5432/rume".to_string()
            ))
        );
    }

    #[test]
    fn resolve_db_url_expands_interpolation_after_merging_env_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "DB_HOST=dev.example\nDATABASE_URL=postgres://app:secret@${DB_HOST}:5432/rume\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".env.production"), "DB_HOST=prod.example\n").unwrap();

        assert_eq!(
            resolve_db_url_for_env_with_vars(dir.path(), "production", std::iter::empty()),
            Ok(Some(
                "postgres://app:secret@prod.example:5432/rume".to_string()
            ))
        );
    }

    #[test]
    fn resolve_db_url_fails_closed_on_dotenv_error_without_toml_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/app.toml"),
            "[database]\nurl = \"sqlite://dev.db\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "DATABASE_URL=postgres://prod:super-secret-pw@prod.example:5432/rume\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".env.production"),
            "SECRET_TOKEN=\"unclosed-secret-value\n",
        )
        .unwrap();

        let err = resolve_db_url_for_env_in(dir.path(), "production")
            .expect_err("malformed .env.production must fail closed instead of falling back");
        assert!(
            !err.contains("unclosed-secret-value") && !err.contains("super-secret-pw"),
            "error must not leak secret values: {err}"
        );
    }

    #[test]
    fn file_value_nonexistent_and_symlink_escape_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join(".env");

        let missing_err = resolve_file_value("file:missing.key".to_string(), dir.path(), &source)
            .expect_err("missing file: target must fail closed during canonicalization");
        assert!(
            missing_err.contains("failed to canonicalize"),
            "expected canonicalization error, got: {missing_err}"
        );

        let missing_dir = dir.path().join("missing_dir");
        let missing_dir_err =
            resolve_file_value("file:secret.key".to_string(), &missing_dir, &source)
                .expect_err("nonexistent workspace dir must fail closed during canonicalization");
        assert!(
            missing_dir_err.contains("failed to canonicalize"),
            "expected canonicalization error, got: {missing_dir_err}"
        );

        #[cfg(unix)]
        {
            let outside_dir = tempfile::tempdir().unwrap();
            let outside_secret = outside_dir.path().join("outside.key");
            std::fs::write(&outside_secret, "secret").unwrap();

            let escape_link = dir.path().join("escape.key");
            std::os::unix::fs::symlink(&outside_secret, &escape_link).unwrap();

            let escape_err = resolve_file_value("file:escape.key".to_string(), dir.path(), &source)
                .expect_err("symlink pointing outside workspace must be rejected");
            assert!(
                escape_err.contains("path escapes workspace directory"),
                "unexpected error: {escape_err}"
            );
        }
    }
}
