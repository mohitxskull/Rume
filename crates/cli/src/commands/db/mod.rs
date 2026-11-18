//! Database migration generation, status inspection, and execution CLI commands.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

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

pub(crate) fn resolve_db_url() -> Option<String> {
    resolve_db_url_in(Path::new("."))
}

pub(crate) fn resolve_db_url_in(dir: &Path) -> Option<String> {
    // Why: route URL resolution through Rume's env/config engines instead of
    // raw `dotenvy::dotenv()` + `toml::Value` parsing so `.env.{env}` layering,
    // `${VAR}` interpolation, `file:` secrets, and `config/app.{env,local}.toml`
    // overrides share a single source of truth with application boot.
    let env_url = match rume::env::ResolvedEnv::load_in(dir) {
        Ok(resolved) => resolved.get("DATABASE_URL").map(str::to_string),
        Err(_) => std::env::var("DATABASE_URL").ok(),
    };
    if let Some(url) = env_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let cfg = rume::config::load_in::<DatabaseFileConfig>(dir).ok()?;
    let url = cfg.database?.url?;
    let trimmed = url.trim();
    if !trimmed.is_empty() {
        Some(trimmed.to_string())
    } else {
        None
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
    let db_url = resolve_db_url();
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
        Some(url) => url,
        None => {
            eprintln!("{}", style("error: DATABASE_URL is not set and database.url was not found in config/app.toml").red());
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
        Some(url) => url,
        None => {
            eprintln!("{}", style("error: DATABASE_URL is not set and database.url was not found in config/app.toml").red());
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
    use super::{BackendHandle, connect_backend, resolve_db_url_in};

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

        // Neither .env nor config/app.toml present -> None
        assert_eq!(resolve_db_url_in(dir.path()), None);

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
            resolve_db_url_in(dir.path()),
            Some("mariadb://local:3306/app".to_string())
        );

        // Empty DATABASE_URL in .env falls through to config/app*.toml
        std::fs::write(dir.path().join(".env"), "DATABASE_URL=   \n").unwrap();
        assert_eq!(
            resolve_db_url_in(dir.path()),
            Some("mariadb://local:3306/app".to_string())
        );

        // .env with ${VAR} interpolation takes precedence over config/app*.toml
        std::fs::write(
            dir.path().join(".env"),
            "DB_HOST=db.internal\nDATABASE_URL=postgres://app:secret@${DB_HOST}:5432/rume\n",
        )
        .unwrap();
        assert_eq!(
            resolve_db_url_in(dir.path()),
            Some("postgres://app:secret@db.internal:5432/rume".to_string())
        );
    }
}
