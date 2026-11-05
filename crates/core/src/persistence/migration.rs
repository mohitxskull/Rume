//! Database migration definitions, runner, and preflight verification.

use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction, DbValue};
use crate::persistence::error::PersistenceError;

/// Canonical migration identifier for Rume core persistence schemas.
pub const RUME_CORE_MIGRATION_ID: &str = "20261104_001_rume_core_schemas";

const PG_OUTBOX: &str = include_str!("sql/postgres/outbox_schema.sql");
const PG_JOBS: &str = include_str!("sql/postgres/jobs_schema.sql");
const PG_LOCKS: &str = include_str!("sql/postgres/locks_schema.sql");
const PG_LEADER: &str = include_str!("sql/postgres/leader_schema.sql");
const PG_CORE_DOWN: &str = include_str!("sql/postgres/core_down.sql");

const MYSQL_OUTBOX: &str = include_str!("sql/mysql/outbox_schema.sql");
const MYSQL_CORE_DOWN: &str = include_str!("sql/mysql/core_down.sql");

const SQLITE_OUTBOX: &str = include_str!("sql/sqlite/outbox_schema.sql");
const SQLITE_CORE_DOWN: &str = include_str!("sql/sqlite/core_down.sql");

const PG_MIGRATIONS_SCHEMA: &str = include_str!("sql/postgres/migrations_schema.sql");
const MYSQL_MIGRATIONS_SCHEMA: &str = include_str!("sql/mysql/migrations_schema.sql");
const SQLITE_MIGRATIONS_SCHEMA: &str = include_str!("sql/sqlite/migrations_schema.sql");

/// Canonical DDL for the migration tracking ledger table across all database backends.
pub const CREATE_MIGRATIONS_TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS rume_migrations (\
     id VARCHAR(255) PRIMARY KEY, \
     name VARCHAR(255) NOT NULL DEFAULT '', \
     applied_at BIGINT NOT NULL, \
     checksum VARCHAR(64) NOT NULL DEFAULT '')";

/// Returns the dialect-specific DDL for creating the `rume_migrations` table.
pub fn migrations_schema_ddl(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATIONS_SCHEMA,
        "mysql" => MYSQL_MIGRATIONS_SCHEMA,
        "sqlite" => SQLITE_MIGRATIONS_SCHEMA,
        _ => CREATE_MIGRATIONS_TABLE_DDL,
    }
}

fn find_checksum_query(dialect: &str) -> &'static str {
    if dialect == "postgres" {
        "SELECT checksum FROM rume_migrations WHERE id = $1"
    } else {
        "SELECT checksum FROM rume_migrations WHERE id = ?"
    }
}

fn insert_migration_query(dialect: &str) -> &'static str {
    if dialect == "postgres" {
        "INSERT INTO rume_migrations (id, name, applied_at, checksum) VALUES ($1, $2, $3, $4)"
    } else {
        "INSERT INTO rume_migrations (id, name, applied_at, checksum) VALUES (?, ?, ?, ?)"
    }
}

fn delete_migration_query(dialect: &str) -> &'static str {
    if dialect == "postgres" {
        "DELETE FROM rume_migrations WHERE id = $1"
    } else {
        "DELETE FROM rume_migrations WHERE id = ?"
    }
}

fn find_migration_id_query(dialect: &str) -> &'static str {
    if dialect == "postgres" {
        "SELECT id FROM rume_migrations WHERE id = $1"
    } else {
        "SELECT id FROM rume_migrations WHERE id = ?"
    }
}

const LIST_APPLIED_MIGRATIONS_SQL: &str =
    "SELECT id, applied_at FROM rume_migrations ORDER BY applied_at ASC";

/// A single versioned database migration step.
#[derive(Clone)]
pub struct Migration {
    /// Unique identifier for this migration (e.g., "20261102_001_create_users").
    pub id: String,
    /// Human-readable description of changes in this migration.
    pub description: String,
    /// Up statement to execute.
    pub up_sql: Option<String>,
    /// Down statement to execute on rollback.
    pub down_sql: Option<String>,
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("id", &self.id)
            .field("description", &self.description)
            .field("up_sql", &self.up_sql)
            .field("down_sql", &self.down_sql)
            .finish()
    }
}

impl Migration {
    /// Creates a new migration with an up statement.
    pub fn from_sql(
        id: impl Into<String>,
        description: impl Into<String>,
        up_sql: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            up_sql: Some(up_sql.into()),
            down_sql: None,
        }
    }

    /// Attaches a down statement for rollback support.
    pub fn with_down(mut self, down_sql: impl Into<String>) -> Self {
        self.down_sql = Some(down_sql.into());
        self
    }

    /// Computes the SHA-256 hexadecimal digest of the migration's `up_sql` statement.
    pub fn checksum(&self) -> String {
        match &self.up_sql {
            Some(s) => {
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(s.as_bytes());
                let mut hex = String::with_capacity(digest.len() * 2);
                for byte in digest {
                    use std::fmt::Write;
                    let _ = write!(hex, "{byte:02x}");
                }
                hex
            }
            None => String::new(),
        }
    }
}

/// Summary report of executed migrations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationSummary {
    /// Identifiers of applied migrations in order of execution.
    pub applied: Vec<String>,
}

/// Migration registry and execution runner.
#[derive(Debug, Default, Clone)]
pub struct Migrator {
    migrations: Vec<Migration>,
}

impl Migrator {
    /// Creates an empty migrator registry.
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    /// Registers the Rume core persistence migration (outbox, jobs, locks, leader) appropriate for the given dialect.
    pub fn with_rume_core(mut self, dialect: &str) -> Self {
        let (up_sql, down_sql) = match dialect {
            "postgres" => {
                let up = format!("{}\n{}\n{}\n{}", PG_OUTBOX, PG_JOBS, PG_LOCKS, PG_LEADER);
                (up, PG_CORE_DOWN)
            }
            "mysql" => (MYSQL_OUTBOX.to_string(), MYSQL_CORE_DOWN),
            "sqlite" => (SQLITE_OUTBOX.to_string(), SQLITE_CORE_DOWN),
            _ => {
                return self;
            }
        };

        self.migrations.insert(
            0,
            Migration::from_sql(
                RUME_CORE_MIGRATION_ID,
                "Initialize Rume core persistence tables",
                up_sql,
            )
            .with_down(down_sql),
        );
        self
    }

    /// Registers a migration.
    pub fn add_migration(mut self, migration: Migration) -> Self {
        self.migrations.push(migration);
        self
    }

    /// Returns registered migrations.
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }

    /// Finds all registered migrations that have not yet been applied to the given database backend.
    pub async fn pending_migrations<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Vec<String>, PersistenceError> {
        let ddl = migrations_schema_ddl(backend.dialect());
        backend.execute(ddl).await?;

        let check_sql = find_checksum_query(backend.dialect());
        let mut pending = Vec::new();
        for migration in &self.migrations {
            let id_param = [DbValue::from(migration.id.as_str())];
            let stored_checksum = backend.query_scalar_params(check_sql, &id_param).await?;
            if let Some(actual) = stored_checksum {
                let expected = migration.checksum();
                if !actual.is_empty() && actual != expected {
                    return Err(PersistenceError::MigrationChecksumMismatch {
                        migration: migration.id.clone(),
                        expected,
                        actual,
                    });
                }
            } else {
                pending.push(migration.id.clone());
            }
        }
        Ok(pending)
    }

    /// Returns the list of applied migration IDs and timestamps from the database.
    pub async fn applied_migrations<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Vec<(String, i64)>, PersistenceError> {
        let ddl = migrations_schema_ddl(backend.dialect());
        backend.execute(ddl).await?;
        let rows = backend.fetch_all(LIST_APPLIED_MIGRATIONS_SQL, &[]).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id")?;
            let applied_at: i64 = row.get("applied_at")?;
            out.push((id, applied_at));
        }
        Ok(out)
    }

    /// Executes all unapplied pending migrations in order on any [`DatabaseBackend`].
    pub async fn run_pending<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<MigrationSummary, PersistenceError> {
        self.run_pending_backend(backend).await
    }

    /// Rolls back the most recently applied migration on any [`DatabaseBackend`].
    ///
    /// Fails closed with [`PersistenceError::IrreversibleMigration`] if the target
    /// migration lacks a `down_sql` rollback script, preserving executed state.
    pub async fn rollback_last<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Option<String>, PersistenceError> {
        self.rollback_last_backend(backend).await
    }

    /// Preflight verification on any [`DatabaseBackend`]: fails closed if any registered migrations have not been applied.
    pub async fn verify_no_pending<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<(), PersistenceError> {
        self.verify_no_pending_backend(backend).await
    }

    /// Verifies checksums of all applied migrations against local definitions on any [`DatabaseBackend`].
    ///
    /// Fails closed with [`PersistenceError::MigrationChecksumMismatch`] if any applied migration has drifted.
    pub async fn verify_applied_checksums<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<(), PersistenceError> {
        let ddl = migrations_schema_ddl(backend.dialect());
        backend.execute(ddl).await?;

        let check_sql = find_checksum_query(backend.dialect());
        for migration in &self.migrations {
            let id_param = [DbValue::from(migration.id.as_str())];
            let stored_checksum = backend.query_scalar_params(check_sql, &id_param).await?;
            if let Some(actual) = stored_checksum {
                let expected = migration.checksum();
                if !actual.is_empty() && actual != expected {
                    return Err(PersistenceError::MigrationChecksumMismatch {
                        migration: migration.id.clone(),
                        expected,
                        actual,
                    });
                }
            }
        }
        Ok(())
    }

    /// Executes all unapplied pending migrations in order on any [`DatabaseBackend`].
    pub async fn run_pending_backend<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<MigrationSummary, PersistenceError> {
        let dialect = backend.dialect();
        let ddl = migrations_schema_ddl(dialect);
        let check_sql = find_checksum_query(dialect);
        let insert_sql = insert_migration_query(dialect);

        if dialect == "postgres" {
            let tx = backend.begin_transaction().await?;
            tx.execute("SELECT pg_advisory_xact_lock(714285723490812734)")
                .await?;
            tx.execute(ddl).await?;

            let mut applied = Vec::new();
            for migration in &self.migrations {
                let id_param = [DbValue::from(migration.id.as_str())];
                let stored_checksum = tx.query_scalar_params(check_sql, &id_param).await?;
                if let Some(actual) = stored_checksum {
                    let expected = migration.checksum();
                    if !actual.is_empty() && actual != expected {
                        return Err(PersistenceError::MigrationChecksumMismatch {
                            migration: migration.id.clone(),
                            expected,
                            actual,
                        });
                    }
                } else {
                    if let Some(sql) = &migration.up_sql {
                        tx.execute(sql)
                            .await
                            .map_err(|err| PersistenceError::MigrationError {
                                migration: migration.id.clone(),
                                message: err.to_string(),
                            })?;
                    }
                    let now = if let Ok(Some(s)) = tx
                        .query_scalar(
                            "SELECT (EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT::text",
                        )
                        .await
                        && let Ok(ts) = s.parse::<u64>()
                    {
                        ts
                    } else {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                    };
                    let params = [
                        DbValue::from(migration.id.as_str()),
                        DbValue::from(migration.description.as_str()),
                        DbValue::Int(now as i64),
                        DbValue::from(migration.checksum().as_str()),
                    ];
                    tx.execute_params(insert_sql, &params).await?;
                    applied.push(migration.id.clone());
                }
            }
            tx.commit().await?;
            return Ok(MigrationSummary { applied });
        }

        backend.execute(ddl).await?;

        let mut applied = Vec::new();
        for migration in &self.migrations {
            let id_param = [DbValue::from(migration.id.as_str())];
            let stored_checksum = backend.query_scalar_params(check_sql, &id_param).await?;
            if let Some(actual) = stored_checksum {
                let expected = migration.checksum();
                if !actual.is_empty() && actual != expected {
                    return Err(PersistenceError::MigrationChecksumMismatch {
                        migration: migration.id.clone(),
                        expected,
                        actual,
                    });
                }
            } else {
                let tx = backend.begin_transaction().await?;
                if let Some(sql) = &migration.up_sql {
                    tx.execute(sql)
                        .await
                        .map_err(|err| PersistenceError::MigrationError {
                            migration: migration.id.clone(),
                            message: err.to_string(),
                        })?;
                }
                let clock_query = match dialect {
                    "mysql" => Some("SELECT UNIX_TIMESTAMP()"),
                    "sqlite" => Some("SELECT CAST(strftime('%s', 'now') AS TEXT)"),
                    _ => None,
                };
                let now = if let Some(query) = clock_query
                    && let Ok(Some(s)) = tx.query_scalar(query).await
                    && let Ok(ts) = s.parse::<u64>()
                {
                    ts
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                };
                let params = [
                    DbValue::from(migration.id.as_str()),
                    DbValue::from(migration.description.as_str()),
                    DbValue::Int(now as i64),
                    DbValue::from(migration.checksum().as_str()),
                ];
                tx.execute_params(insert_sql, &params).await?;
                tx.commit().await?;
                applied.push(migration.id.clone());
            }
        }

        Ok(MigrationSummary { applied })
    }

    /// Rolls back the most recently applied migration on any [`DatabaseBackend`].
    pub async fn rollback_last_backend<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<Option<String>, PersistenceError> {
        let dialect = backend.dialect();
        let ddl = migrations_schema_ddl(dialect);
        let check_sql = find_migration_id_query(dialect);
        let delete_sql = delete_migration_query(dialect);

        if dialect == "postgres" {
            let tx = backend.begin_transaction().await?;
            tx.execute("SELECT pg_advisory_xact_lock(714285723490812734)")
                .await?;
            tx.execute(ddl).await?;

            for migration in self.migrations.iter().rev() {
                let id_param = [DbValue::from(migration.id.as_str())];
                let applied = tx.query_scalar_params(check_sql, &id_param).await?;
                if applied.is_some() {
                    let down_sql = migration.down_sql.as_deref().ok_or_else(|| {
                        PersistenceError::IrreversibleMigration(migration.id.clone())
                    })?;
                    tx.execute(down_sql)
                        .await
                        .map_err(|err| PersistenceError::MigrationError {
                            migration: migration.id.clone(),
                            message: err.to_string(),
                        })?;
                    tx.execute_params(delete_sql, &id_param).await?;
                    tx.commit().await?;
                    return Ok(Some(migration.id.clone()));
                }
            }
            tx.commit().await?;
            return Ok(None);
        }

        backend.execute(ddl).await?;

        for migration in self.migrations.iter().rev() {
            let id_param = [DbValue::from(migration.id.as_str())];
            let exists = backend.query_scalar_params(check_sql, &id_param).await?;
            if exists.is_some() {
                let Some(sql) = &migration.down_sql else {
                    return Err(PersistenceError::IrreversibleMigration(
                        migration.id.clone(),
                    ));
                };

                let tx = backend.begin_transaction().await?;
                tx.execute(sql)
                    .await
                    .map_err(|err| PersistenceError::MigrationError {
                        migration: migration.id.clone(),
                        message: err.to_string(),
                    })?;

                tx.execute_params(delete_sql, &id_param).await?;
                tx.commit().await?;
                return Ok(Some(migration.id.clone()));
            }
        }

        Ok(None)
    }

    /// Preflight verification on any [`DatabaseBackend`]: fails closed if any registered migrations have not been applied.
    pub async fn verify_no_pending_backend<B: DatabaseBackend>(
        &self,
        backend: &B,
    ) -> Result<(), PersistenceError> {
        let ddl = migrations_schema_ddl(backend.dialect());
        backend.execute(ddl).await?;

        let check_sql = find_checksum_query(backend.dialect());
        let mut pending = Vec::new();
        for migration in &self.migrations {
            let id_param = [DbValue::from(migration.id.as_str())];
            let stored_checksum = backend.query_scalar_params(check_sql, &id_param).await?;
            if let Some(actual) = stored_checksum {
                let expected = migration.checksum();
                if !actual.is_empty() && actual != expected {
                    return Err(PersistenceError::MigrationChecksumMismatch {
                        migration: migration.id.clone(),
                        expected,
                        actual,
                    });
                }
            } else {
                pending.push(migration.id.clone());
            }
        }

        if pending.is_empty() {
            Ok(())
        } else {
            Err(PersistenceError::PendingMigrations(pending))
        }
    }
}
