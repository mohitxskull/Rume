//! Database migration generation, status inspection, and execution CLI commands.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use comfy_table::Color;
use console::style;
use rume::persistence::config::DatabaseConfig;
use rume::persistence::migration::{Migration, Migrator};
use rume::persistence::seed::{SeedRegistry, SeedReport, SqlFileSeeder};
use rume::persistence::{
    DatabaseBackend, DatabasePool, MysqlBackend, PersistenceError, PostgresBackend, SqliteBackend,
};

use crate::cli::DbCommands;
use crate::ui::table::TableBuilder;

async fn run_registry<B: DatabaseBackend>(
    backend: B,
    config: DatabaseConfig,
    registry: &SeedRegistry,
    only: Option<&[&str]>,
) -> Result<SeedReport, PersistenceError> {
    let pool = DatabasePool::new(backend, config);
    match only {
        Some(names) => registry.run_only(&pool, names).await,
        None => registry.run_all(&pool).await,
    }
}

pub async fn run(cmd: DbCommands) {
    match cmd {
        DbCommands::Status => status().await,
        DbCommands::Migrate => migrate().await,
        DbCommands::Rollback => rollback().await,
        DbCommands::Seed { path, only } => seed(path, only).await,
    }
}

pub async fn seed(path: Option<String>, only: Option<Vec<String>>) {
    let explicit_path = path.is_some();
    let seeds_dir_str = path.unwrap_or_else(|| "seeds".to_string());
    let seeds_dir = Path::new(&seeds_dir_str);

    if !seeds_dir.exists() {
        if explicit_path {
            eprintln!(
                "{}",
                style(format!(
                    "error: seeds directory '{}' does not exist",
                    seeds_dir.display()
                ))
                .red()
            );
            std::process::exit(1);
        }
        println!("No seeds found in {}/ directory.", seeds_dir.display());
        return;
    }

    let mut sql_files: BTreeMap<String, String> = BTreeMap::new();
    let entries = match fs::read_dir(seeds_dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!(
                "{}",
                style(format!(
                    "error: failed to read seeds directory '{}': {e}",
                    seeds_dir.display()
                ))
                .red()
            );
            std::process::exit(1);
        }
    };

    for entry in entries.filter_map(Result::ok) {
        let file_path = entry.path();
        if !file_path.is_file() {
            continue;
        }
        if file_path.extension().and_then(|ext| ext.to_str()) != Some("sql") {
            continue;
        }
        let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "{}",
                    style(format!(
                        "error: failed to read seed file '{}': {e}",
                        file_path.display()
                    ))
                    .red()
                );
                std::process::exit(1);
            }
        };
        sql_files.insert(stem.to_string(), content);
    }

    if sql_files.is_empty() {
        println!(
            "No .sql seed files found in {}/ directory.",
            seeds_dir.display()
        );
        return;
    }

    let mut registry = SeedRegistry::new();
    for (name, sql) in sql_files {
        registry.register(SqlFileSeeder::new(name, sql));
    }

    let db_url = match resolve_db_url() {
        Ok(Some(url)) => url,
        Ok(None) => {
            eprintln!(
                "{}",
                style("error: DATABASE_URL is not set and database.url was not found in config/app.toml").red()
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("{}", style(format!("error: {err}")).red());
            std::process::exit(1);
        }
    };

    let (handle, _dialect) = match connect_backend(&db_url).await {
        Ok(res) => res,
        Err(err) => {
            eprintln!("{}", style(err).red());
            std::process::exit(1);
        }
    };

    let config = DatabaseConfig::new(&db_url);
    let only_refs: Option<Vec<&str>> = only
        .as_ref()
        .map(|names| names.iter().map(String::as_str).collect());

    let report_res: Result<SeedReport, PersistenceError> = match handle {
        BackendHandle::Postgres(b) => {
            run_registry(b, config, &registry, only_refs.as_deref()).await
        }
        BackendHandle::Sqlite(b) => run_registry(b, config, &registry, only_refs.as_deref()).await,
        BackendHandle::Mysql(b) => run_registry(b, config, &registry, only_refs.as_deref()).await,
    };

    match report_res {
        Ok(report) => {
            let mut builder = TableBuilder::new();
            builder.head(&["Seed", "Status"]);
            for name in &report.applied {
                builder.row_colored(&[(name, None), ("Applied", Some(Color::Green))]);
            }
            for name in &report.skipped {
                builder.row_colored(&[(name, None), ("Skipped", Some(Color::Yellow))]);
            }
            if report.applied.is_empty() && report.skipped.is_empty() {
                println!("No seeds executed.");
            } else {
                builder.render();
            }
        }
        Err(e) => {
            eprintln!("{}", style(format!("error: seeding failed: {e}")).red());
            std::process::exit(1);
        }
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
    // Why: delegate `.env*` layering, `file:` resolution, and post-merge interpolation
    // to `rume::env::ResolvedEnv::load_for_env_with_vars` (single source of truth).
    // Process-only `DATABASE_URL` values are not interpolated by `ResolvedEnv`, preserving
    // literal `${...}` sequences in passwords, and `.env` parse/interpolation errors fail
    // closed instead of silently falling back to `config/app*.toml`.
    let resolved = rume::env::ResolvedEnv::load_for_env_with_vars(dir, env_name, process_vars)
        .map_err(|e| format!("failed to resolve environment: {e}"))?;

    if let Some(url) = resolved.get("DATABASE_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }

    match rume::config::load_for_env_in::<DatabaseFileConfig>(dir, env_name) {
        Ok(cfg) => {
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
        Err(rume::config::ConfigError::NotFound { .. }) => Ok(None),
        Err(err) => Err(format!("failed to load database configuration: {err}")),
    }
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
        BackendHandle, connect_backend, resolve_db_url_for_env_in, resolve_db_url_for_env_with_vars,
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
        std::fs::write(dir.path().join(".env"), "DATABASE_URL=file:missing.key\n").unwrap();

        let missing_err =
            resolve_db_url_for_env_with_vars(dir.path(), "development", std::iter::empty())
                .expect_err("missing file: target must fail closed");
        assert!(
            missing_err.contains("failed to resolve environment"),
            "unexpected error: {missing_err}"
        );

        #[cfg(unix)]
        {
            let outside_dir = tempfile::tempdir().unwrap();
            let outside_secret = outside_dir.path().join("outside.key");
            std::fs::write(&outside_secret, "sqlite::memory:").unwrap();

            let escape_link = dir.path().join("escape.key");
            std::os::unix::fs::symlink(&outside_secret, &escape_link).unwrap();
            std::fs::write(dir.path().join(".env"), "DATABASE_URL=file:escape.key\n").unwrap();

            let escape_err =
                resolve_db_url_for_env_with_vars(dir.path(), "development", std::iter::empty())
                    .expect_err("symlink pointing outside workspace must be rejected");
            assert!(
                escape_err.contains("escapes directory"),
                "unexpected error: {escape_err}"
            );
        }
    }

    #[test]
    fn cli_parses_db_seed_and_alias() {
        use crate::cli::{Cli, Commands, DbCommands};
        use clap::Parser;

        let parsed = Cli::try_parse_from([
            "rume",
            "db",
            "seed",
            "--path",
            "custom/seeds",
            "--only",
            "users,roles",
        ])
        .expect("rume db seed should parse --path and --only");
        match parsed.command {
            Some(Commands::Db {
                command: DbCommands::Seed { path, only },
            }) => {
                assert_eq!(path.as_deref(), Some("custom/seeds"));
                assert_eq!(only, Some(vec!["users".to_string(), "roles".to_string()]));
            }
            _ => panic!("expected DbCommands::Seed"),
        }

        let alias_parsed =
            Cli::try_parse_from(["rume", "db:seed", "--path", "seeds", "--only", "users"])
                .expect("rume db:seed alias should parse --path and --only");
        match alias_parsed.command {
            Some(Commands::DbSeed { path, only }) => {
                assert_eq!(path.as_deref(), Some("seeds"));
                assert_eq!(only, Some(vec!["users".to_string()]));
            }
            _ => panic!("expected Commands::DbSeed"),
        }
    }

    #[test]
    fn resolve_db_url_preserves_literal_interpolation_in_process_only_password() {
        let dir = tempfile::tempdir().unwrap();
        let url = "postgres://app:p${secret}w@db.internal:5432/rume";
        let resolved = resolve_db_url_for_env_with_vars(
            dir.path(),
            "production",
            vec![("DATABASE_URL".to_string(), url.to_string())],
        );
        assert_eq!(resolved, Ok(Some(url.to_string())));
    }

    #[test]
    fn resolve_db_url_surfaces_toml_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/app.toml"),
            "[database\nurl = \"sqlite://broken.db\"\n",
        )
        .unwrap();

        let err = resolve_db_url_for_env_with_vars(dir.path(), "development", std::iter::empty())
            .expect_err("malformed config/app.toml must return an error instead of Ok(None)");
        assert!(
            err.contains("database configuration") || err.contains("configuration"),
            "unexpected error: {err}"
        );
    }
}
