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
pub const CREATE_MIGRATIONS_TABLE_DDL: &str = PG_MIGRATIONS_SCHEMA;

/// Returns the dialect-specific DDL for creating the `rume_migrations` table.
pub fn migrations_schema_ddl(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATIONS_SCHEMA,
        "mysql" => MYSQL_MIGRATIONS_SCHEMA,
        "sqlite" => SQLITE_MIGRATIONS_SCHEMA,
        _ => CREATE_MIGRATIONS_TABLE_DDL,
    }
}

// Per-dialect migration query SQL, externalized from inline string literals.
const PG_MIGRATION_FIND_CHECKSUM: &str = include_str!("sql/postgres/migration_find_checksum.sql");
const PG_MIGRATION_INSERT: &str = include_str!("sql/postgres/migration_insert.sql");
const PG_MIGRATION_DELETE: &str = include_str!("sql/postgres/migration_delete.sql");
const PG_MIGRATION_FIND_ID: &str = include_str!("sql/postgres/migration_find_id.sql");
const PG_MIGRATION_LIST_APPLIED: &str = include_str!("sql/common/migration_list_applied.sql");
const PG_ADVISORY_LOCK: &str = include_str!("sql/postgres/pg_advisory_lock.sql");
const PG_CLOCK: &str = include_str!("sql/postgres/pg_clock.sql");

const MYSQL_MIGRATION_FIND_CHECKSUM: &str = include_str!("sql/mysql/migration_find_checksum.sql");
const MYSQL_MIGRATION_INSERT: &str = include_str!("sql/mysql/migration_insert.sql");
const MYSQL_MIGRATION_DELETE: &str = include_str!("sql/mysql/migration_delete.sql");
const MYSQL_MIGRATION_FIND_ID: &str = include_str!("sql/mysql/migration_find_id.sql");
const MYSQL_CLOCK: &str = include_str!("sql/mysql/mysql_clock.sql");
const MYSQL_MIGRATION_LOCK: &str = include_str!("sql/mysql/mysql_migration_lock.sql");
const MYSQL_MIGRATION_UNLOCK: &str = include_str!("sql/mysql/mysql_migration_unlock.sql");

const SQLITE_MIGRATION_FIND_CHECKSUM: &str = include_str!("sql/sqlite/migration_find_checksum.sql");
const SQLITE_MIGRATION_INSERT: &str = include_str!("sql/sqlite/migration_insert.sql");
const SQLITE_MIGRATION_DELETE: &str = include_str!("sql/sqlite/migration_delete.sql");
const SQLITE_MIGRATION_FIND_ID: &str = include_str!("sql/sqlite/migration_find_id.sql");
const SQLITE_CLOCK: &str = include_str!("sql/sqlite/sqlite_clock.sql");
const SQLITE_BUSY_TIMEOUT: &str = include_str!("sql/sqlite/sqlite_busy_timeout.sql");

fn find_checksum_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_FIND_CHECKSUM,
        "mysql" => MYSQL_MIGRATION_FIND_CHECKSUM,
        _ => SQLITE_MIGRATION_FIND_CHECKSUM,
    }
}

fn insert_migration_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_INSERT,
        "mysql" => MYSQL_MIGRATION_INSERT,
        _ => SQLITE_MIGRATION_INSERT,
    }
}

fn delete_migration_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_DELETE,
        "mysql" => MYSQL_MIGRATION_DELETE,
        _ => SQLITE_MIGRATION_DELETE,
    }
}

fn find_migration_id_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_FIND_ID,
        "mysql" => MYSQL_MIGRATION_FIND_ID,
        _ => SQLITE_MIGRATION_FIND_ID,
    }
}

const LIST_APPLIED_MIGRATIONS_SQL: &str = PG_MIGRATION_LIST_APPLIED;

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
    ///
    /// For no-SQL migrations (`up_sql == None`), returns a deterministic non-empty sentinel digest
    /// so ledger verification never mistakes an intentional no-SQL migration for a corrupt empty checksum.
    pub fn checksum(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = match &self.up_sql {
            Some(s) => s.as_bytes(),
            None => b"__rume_no_sql_migration__",
        };
        let digest = Sha256::digest(bytes);
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
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
                if actual.is_empty() || actual != expected {
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
                if actual.is_empty() || actual != expected {
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
            tx.execute(PG_ADVISORY_LOCK).await?;
            tx.execute(ddl).await?;

            let mut applied = Vec::new();
            for migration in &self.migrations {
                let id_param = [DbValue::from(migration.id.as_str())];
                let stored_checksum = tx.query_scalar_params(check_sql, &id_param).await?;
                if let Some(actual) = stored_checksum {
                    let expected = migration.checksum();
                    if actual.is_empty() || actual != expected {
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
                    let now = if let Ok(Some(s)) = tx.query_scalar(PG_CLOCK).await
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

        if dialect == "mysql" {
            let tx = backend.begin_transaction().await?;
            let lock_res = tx.query_scalar(MYSQL_MIGRATION_LOCK).await?;
            if lock_res.as_deref() != Some("1") {
                return Err(PersistenceError::Timeout);
            }
            let mut applied = Vec::new();
            let run_res: Result<(), PersistenceError> = async {
                // `CREATE TABLE IF NOT EXISTS` triggers an implicit commit in MySQL, so run it
                // under the named lock first and immediately start a fresh transaction for data
                // migrations and ledger inserts.
                tx.execute(ddl).await?;
                tx.execute("START TRANSACTION").await?;
                for migration in &self.migrations {
                    let id_param = [DbValue::from(migration.id.as_str())];
                    let stored_checksum = tx.query_scalar_params(check_sql, &id_param).await?;
                    if let Some(actual) = stored_checksum {
                        let expected = migration.checksum();
                        if actual.is_empty() || actual != expected {
                            return Err(PersistenceError::MigrationChecksumMismatch {
                                migration: migration.id.clone(),
                                expected,
                                actual,
                            });
                        }
                    } else {
                        if let Some(sql) = &migration.up_sql {
                            tx.execute(sql).await.map_err(|err| {
                                PersistenceError::MigrationError {
                                    migration: migration.id.clone(),
                                    message: err.to_string(),
                                }
                            })?;
                        }
                        let now = if let Ok(Some(s)) = tx.query_scalar(MYSQL_CLOCK).await
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
                Ok(())
            }
            .await;

            if let Err(err) = run_res {
                // If migration execution failed, rollback changes and release the named lock before returning
                let _ = tx.execute("ROLLBACK").await;
                let _ = tx.query_scalar(MYSQL_MIGRATION_UNLOCK).await;
                let _ = tx.rollback().await;
                return Err(err);
            }

            // CRITICAL MYSQL COORDINATION ORDERING (DO NOT REORDER):
            // In MySQL, `GET_LOCK` and `RELEASE_LOCK` are bound to the specific server thread/connection (`CONNECTION_ID()`).
            //
            // 1. Durably commit the transaction:
            //    We execute `COMMIT` directly on the active connection so that all migration schema and ledger updates
            //    are 100% committed before another instance can acquire the lock.
            //    (Releasing the lock before commit would allow a concurrent instance to acquire `rume_migrations_lock`
            //    and read uncommitted ledger rows or attempt duplicate DDL execution).
            tx.execute("COMMIT").await?;

            // 2. Release the MySQL named lock on the EXACT SAME connection while it is still active:
            //    If RELEASE_LOCK were called on a different pooled connection, MySQL would return 0 and refuse to release it.
            let unlock_res = tx.query_scalar(MYSQL_MIGRATION_UNLOCK).await;

            // 3. Finalize the sqlx transaction handle and return connection to the pool:
            let commit_res = tx.commit().await;

            // 4. Propagate commit errors (never discard with `let _ = tx.commit().await`):
            commit_res?;

            // 5. Propagate unlock errors:
            match unlock_res {
                Ok(Some(s)) if s == "1" => {}
                Ok(other) => {
                    return Err(PersistenceError::TransactionError(format!(
                        "failed to release MySQL migration lock: RELEASE_LOCK returned {other:?}"
                    )));
                }
                Err(err) => return Err(err),
            }

            return Ok(MigrationSummary { applied });
        }

        if dialect == "sqlite" {
            let tx = backend.begin_transaction().await?;
            // Set PRAGMA busy_timeout on the exact transaction connection rather than an arbitrary pooled connection.
            tx.execute(SQLITE_BUSY_TIMEOUT).await?;
            tx.execute(ddl).await?;

            let mut applied = Vec::new();
            for migration in &self.migrations {
                let id_param = [DbValue::from(migration.id.as_str())];
                let stored_checksum = tx.query_scalar_params(check_sql, &id_param).await?;
                if let Some(actual) = stored_checksum {
                    let expected = migration.checksum();
                    if actual.is_empty() || actual != expected {
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
                    let now = if let Ok(Some(s)) = tx.query_scalar(SQLITE_CLOCK).await
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
                if actual.is_empty() || actual != expected {
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
                    "mysql" => Some(MYSQL_CLOCK),
                    "sqlite" => Some(SQLITE_CLOCK),
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
            tx.execute(PG_ADVISORY_LOCK).await?;
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

        if dialect == "mysql" {
            let tx = backend.begin_transaction().await?;
            let lock_res = tx.query_scalar(MYSQL_MIGRATION_LOCK).await?;
            if lock_res.as_deref() != Some("1") {
                return Err(PersistenceError::Timeout);
            }
            let mut rolled_back = None;
            let run_res: Result<(), PersistenceError> = async {
                tx.execute(ddl).await?;
                tx.execute("START TRANSACTION").await?;
                for migration in self.migrations.iter().rev() {
                    let id_param = [DbValue::from(migration.id.as_str())];
                    let applied = tx.query_scalar_params(check_sql, &id_param).await?;
                    if applied.is_some() {
                        let down_sql = migration.down_sql.as_deref().ok_or_else(|| {
                            PersistenceError::IrreversibleMigration(migration.id.clone())
                        })?;
                        tx.execute(down_sql).await.map_err(|err| {
                            PersistenceError::MigrationError {
                                migration: migration.id.clone(),
                                message: err.to_string(),
                            }
                        })?;
                        tx.execute_params(delete_sql, &id_param).await?;
                        rolled_back = Some(migration.id.clone());
                        break;
                    }
                }
                Ok(())
            }
            .await;

            if let Err(err) = run_res {
                // If rollback execution failed, rollback changes and release the named lock before returning
                let _ = tx.execute("ROLLBACK").await;
                let _ = tx.query_scalar(MYSQL_MIGRATION_UNLOCK).await;
                let _ = tx.rollback().await;
                return Err(err);
            }

            // CRITICAL MYSQL COORDINATION ORDERING (DO NOT REORDER):
            // 1. Durably commit rollback changes on the active connection before releasing the lock:
            tx.execute("COMMIT").await?;

            // 2. Release the MySQL named lock on the EXACT SAME connection while it is still active:
            let unlock_res = tx.query_scalar(MYSQL_MIGRATION_UNLOCK).await;

            // 3. Finalize the sqlx transaction handle and return connection to the pool:
            let commit_res = tx.commit().await;

            // 4. Propagate commit errors:
            commit_res?;

            // 5. Propagate unlock errors:
            match unlock_res {
                Ok(Some(s)) if s == "1" => {}
                Ok(other) => {
                    return Err(PersistenceError::TransactionError(format!(
                        "failed to release MySQL migration lock: RELEASE_LOCK returned {other:?}"
                    )));
                }
                Err(err) => return Err(err),
            }

            return Ok(rolled_back);
        }

        if dialect == "sqlite" {
            let tx = backend.begin_transaction().await?;
            // Set PRAGMA busy_timeout on the exact transaction connection rather than an arbitrary pooled connection.
            tx.execute(SQLITE_BUSY_TIMEOUT).await?;
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
                if actual.is_empty() || actual != expected {
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
