//! Database migration definitions, runner, and preflight verification.

use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction, DbValue};
use crate::persistence::error::PersistenceError;

/// Canonical migration identifier for Rume core persistence schemas.
pub const RUME_CORE_MIGRATION_ID: &str = "20261104_001_rume_core_schemas";

/// Canonical migration identifier for the Rume audit log schema.
pub const RUME_AUDIT_MIGRATION_ID: &str = "20261104_002_rume_audit_schema";

const PG_OUTBOX: &str = include_str!("sql/postgres/outbox_schema.sql");
const PG_JOBS: &str = include_str!("sql/postgres/jobs_schema.sql");
const PG_LOCKS: &str = include_str!("sql/postgres/locks_schema.sql");
const PG_LEADER: &str = include_str!("sql/postgres/leader_schema.sql");
const PG_AUDIT: &str = include_str!("sql/postgres/audit_schema.sql");
const PG_CORE_DOWN: &str = include_str!("sql/postgres/core_down.sql");

const MYSQL_OUTBOX: &str = include_str!("sql/mysql/outbox_schema.sql");
const MYSQL_AUDIT: &str = include_str!("sql/mysql/audit_schema.sql");
const MYSQL_CORE_DOWN: &str = include_str!("sql/mysql/core_down.sql");

const SQLITE_OUTBOX: &str = include_str!("sql/sqlite/outbox_schema.sql");
const SQLITE_AUDIT: &str = include_str!("sql/sqlite/audit_schema.sql");
const SQLITE_CORE_DOWN: &str = include_str!("sql/sqlite/core_down.sql");

const AUDIT_DOWN_SQL: &str = "DROP TABLE IF EXISTS rume_audit_log;";

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
#[cfg(feature = "mysql")]
const MYSQL_MIGRATION_LOCK: &str = include_str!("sql/mysql/mysql_migration_lock.sql");
#[cfg(feature = "mysql")]
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

fn parse_clock_or_now(clock_res: Result<Option<String>, PersistenceError>) -> i64 {
    let now = if let Ok(Some(s)) = clock_res
        && let Ok(ts) = s.parse::<u64>()
    {
        ts
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    };
    now as i64
}

#[cfg(feature = "mysql")]
fn bind_mysql_conn_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    params: &'q [DbValue],
) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    for param in params {
        query = match param {
            DbValue::Null => query.bind(None as Option<String>),
            DbValue::Text(s) => query.bind(s.as_str()),
            DbValue::Int(i) => query.bind(*i),
            DbValue::Float(f) => query.bind(*f),
            DbValue::Bool(b) => query.bind(*b),
            DbValue::Bytes(b) => query.bind(b.as_slice()),
        };
    }
    query
}

#[cfg(feature = "mysql")]
fn extract_mysql_scalar(row: &sqlx::mysql::MySqlRow) -> Option<String> {
    use sqlx::Row;
    if let Ok(val) = row.try_get::<String, _>(0) {
        return Some(val);
    }
    if let Ok(val) = row.try_get::<i64, _>(0) {
        return Some(val.to_string());
    }
    if let Ok(val) = row.try_get::<u64, _>(0) {
        return Some(val.to_string());
    }
    if let Ok(val) = row.try_get::<i32, _>(0) {
        return Some(val.to_string());
    }
    if let Ok(val) = row.try_get::<bool, _>(0) {
        return Some(val.to_string());
    }
    None
}

#[cfg(feature = "mysql")]
async fn mysql_conn_execute_raw(
    conn: &mut sqlx::MySqlConnection,
    sql: &str,
) -> Result<u64, PersistenceError> {
    let res = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(conn)
        .await
        .map_err(crate::persistence::backend::map_sqlx_error)?;
    Ok(res.rows_affected())
}

#[cfg(feature = "mysql")]
async fn mysql_conn_execute_params(
    conn: &mut sqlx::MySqlConnection,
    sql: &str,
    params: &[DbValue],
) -> Result<u64, PersistenceError> {
    let query = bind_mysql_conn_params(sqlx::query(sqlx::AssertSqlSafe(sql)), params);
    let res = query
        .execute(conn)
        .await
        .map_err(crate::persistence::backend::map_sqlx_error)?;
    Ok(res.rows_affected())
}

#[cfg(feature = "mysql")]
async fn mysql_conn_query_scalar_params(
    conn: &mut sqlx::MySqlConnection,
    sql: &str,
    params: &[DbValue],
) -> Result<Option<String>, PersistenceError> {
    let query = bind_mysql_conn_params(sqlx::query(sqlx::AssertSqlSafe(sql.trim())), params);
    let row = query
        .fetch_optional(conn)
        .await
        .map_err(crate::persistence::backend::map_sqlx_error)?;
    Ok(row.and_then(|r| extract_mysql_scalar(&r)))
}

/// RAII guard owning a pinned MySQL pool connection while `rume_migrations_lock` is held.
///
/// Why: MySQL `GET_LOCK`/`RELEASE_LOCK` are session-bound (`CONNECTION_ID()`), and MySQL DDL
/// implicitly auto-commits every statement. Pinning a single `PoolConnection<MySql>` directly via
/// `MysqlBackend::pool().acquire()` keeps lock acquisition, per-migration execution, and lock
/// release on the same physical session without calling `begin_transaction()`, issuing misleading
/// multi-migration `"START TRANSACTION"`/`"COMMIT"`/`"ROLLBACK"` wrappers, or adding
/// `MysqlTransaction::execute_unvalidated` (revisit only if targeting a transactional-DDL MySQL
/// fork).
#[cfg(feature = "mysql")]
struct MysqlMigrationLockGuard {
    conn: Option<sqlx::pool::PoolConnection<sqlx::MySql>>,
    locked: bool,
}

#[cfg(feature = "mysql")]
impl MysqlMigrationLockGuard {
    async fn acquire(pool: &sqlx::MySqlPool) -> Result<Self, PersistenceError> {
        let conn = pool
            .acquire()
            .await
            .map_err(crate::persistence::backend::map_sqlx_error)?;
        let mut guard = Self {
            conn: Some(conn),
            locked: false,
        };
        let lock_res =
            mysql_conn_query_scalar_params(guard.conn_mut(), MYSQL_MIGRATION_LOCK, &[]).await?;
        if lock_res.as_deref() != Some("1") {
            return Err(PersistenceError::Timeout);
        }
        guard.locked = true;
        Ok(guard)
    }

    fn conn_mut(&mut self) -> &mut sqlx::MySqlConnection {
        self.conn
            .as_mut()
            .expect("MySQL migration connection already released")
    }

    async fn release(mut self) -> Result<(), PersistenceError> {
        if !self.locked {
            return Ok(());
        }
        let unlock_res =
            mysql_conn_query_scalar_params(self.conn_mut(), MYSQL_MIGRATION_UNLOCK, &[]).await;
        // Why: Disarm only after awaiting `RELEASE_LOCK` so cancellation mid-unlock still
        // triggers `Drop` cleanup, while completed calls never spawn a redundant unlock task.
        self.locked = false;
        let _ = self.conn.take();

        match unlock_res {
            Ok(Some(s)) if s == "1" => Ok(()),
            Ok(other) => Err(PersistenceError::TransactionError(format!(
                "failed to release MySQL migration lock: RELEASE_LOCK returned {other:?}"
            ))),
            Err(err) => Err(err),
        }
    }
}

#[cfg(feature = "mysql")]
impl Drop for MysqlMigrationLockGuard {
    fn drop(&mut self) {
        if self.locked
            && let Some(mut conn) = self.conn.take()
        {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // Why: Cancellation-safe cleanup matching `MysqlTransaction::drop`; if the
                // migration future is dropped while locked, release `rume_migrations_lock` on
                // this exact session before returning the connection to the pool.
                handle.spawn(async move {
                    let _ = sqlx::query(MYSQL_MIGRATION_UNLOCK)
                        .execute(&mut *conn)
                        .await;
                });
            } else {
                // Why: Outside a Tokio runtime we cannot spawn `RELEASE_LOCK`; detaching closes
                // the physical socket immediately so MySQL drops the session lock instead of
                // returning a still-locked session to the pool.
                drop(conn.detach());
            }
        }
    }
}

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

    /// Registers the Rume core persistence and audit log migrations appropriate for the given dialect.
    pub fn with_rume_core(mut self, dialect: &str) -> Self {
        // Why: `20261104_001_rume_core_schemas` `up_sql` must stay byte-for-byte immutable so
        // already-deployed databases do not fail checksum verification with
        // `MigrationChecksumMismatch`; `rume_audit_log` is registered as append-only
        // `20261104_002_rume_audit_schema` at index 1 instead of mutating `001`, and
        // `core_down.sql` never drops `rume_migrations` or `rume_audit_log` (revisit only on a
        // breaking major-version schema reset with an explicit ledger rewrite).
        let (core_up_sql, core_down_sql, audit_up_sql) = match dialect {
            "postgres" => {
                let up = format!("{}\n{}\n{}\n{}", PG_OUTBOX, PG_JOBS, PG_LOCKS, PG_LEADER);
                (up, PG_CORE_DOWN, PG_AUDIT)
            }
            "mysql" => (MYSQL_OUTBOX.to_string(), MYSQL_CORE_DOWN, MYSQL_AUDIT),
            "sqlite" => (SQLITE_OUTBOX.to_string(), SQLITE_CORE_DOWN, SQLITE_AUDIT),
            _ => {
                return self;
            }
        };

        self.migrations.insert(
            0,
            Migration::from_sql(
                RUME_CORE_MIGRATION_ID,
                "Initialize Rume core persistence tables",
                core_up_sql,
            )
            .with_down(core_down_sql),
        );
        self.migrations.insert(
            1,
            Migration::from_sql(
                RUME_AUDIT_MIGRATION_ID,
                "Initialize Rume audit log table",
                audit_up_sql,
            )
            .with_down(AUDIT_DOWN_SQL),
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
            // Why: MySQL DDL implicitly commits every statement, so an outer multi-migration
            // `START TRANSACTION` / `COMMIT` / `ROLLBACK` wrapper is non-atomic and causes ledger
            // drift. Downcast to `MysqlBackend` to execute each migration and its immediate
            // `rume_migrations` insert on one `GET_LOCK`-pinned `PoolConnection<MySql>` without
            // `begin_transaction()` or transaction wrappers (revisit only if targeting a
            // transactional-DDL MySQL fork); non-`MysqlBackend` mocks fall back to direct
            // non-transactional `backend` calls.
            #[cfg(feature = "mysql")]
            if let Some(mysql_backend) =
                (backend as &dyn std::any::Any).downcast_ref::<crate::persistence::MysqlBackend>()
            {
                let mut guard = MysqlMigrationLockGuard::acquire(mysql_backend.pool()).await?;
                let mut applied = Vec::new();
                let run_res: Result<(), PersistenceError> = async {
                    let conn = guard.conn_mut();
                    mysql_conn_execute_raw(conn, ddl).await?;
                    for migration in &self.migrations {
                        let id_param = [DbValue::from(migration.id.as_str())];
                        let stored_checksum =
                            mysql_conn_query_scalar_params(conn, check_sql, &id_param).await?;
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
                                mysql_conn_execute_raw(conn, sql).await.map_err(|err| {
                                    PersistenceError::MigrationError {
                                        migration: migration.id.clone(),
                                        message: err.to_string(),
                                    }
                                })?;
                            }
                            let now = parse_clock_or_now(
                                mysql_conn_query_scalar_params(conn, MYSQL_CLOCK, &[]).await,
                            );
                            let params = [
                                DbValue::from(migration.id.as_str()),
                                DbValue::from(migration.description.as_str()),
                                DbValue::Int(now),
                                DbValue::from(migration.checksum().as_str()),
                            ];
                            mysql_conn_execute_params(conn, insert_sql, &params).await?;
                            applied.push(migration.id.clone());
                        }
                    }
                    Ok(())
                }
                .await;

                return match run_res {
                    Ok(()) => {
                        guard.release().await?;
                        Ok(MigrationSummary { applied })
                    }
                    Err(err) => {
                        // Why: Explicitly release the session-bound lock before returning `Err`
                        // so callers probing `IS_FREE_LOCK` immediately never race with `Drop`.
                        let _ = guard.release().await;
                        Err(err)
                    }
                };
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
                    if let Some(sql) = &migration.up_sql {
                        backend.execute(sql).await.map_err(|err| {
                            PersistenceError::MigrationError {
                                migration: migration.id.clone(),
                                message: err.to_string(),
                            }
                        })?;
                    }
                    let now = parse_clock_or_now(backend.query_scalar(MYSQL_CLOCK).await);
                    let params = [
                        DbValue::from(migration.id.as_str()),
                        DbValue::from(migration.description.as_str()),
                        DbValue::Int(now),
                        DbValue::from(migration.checksum().as_str()),
                    ];
                    backend.execute_params(insert_sql, &params).await?;
                    applied.push(migration.id.clone());
                }
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
            // Why: See `run_pending_backend` MySQL branch — MySQL DDL auto-commits per statement,
            // so `rollback_last_backend` pins one `PoolConnection<MySql>` under `GET_LOCK` and
            // deletes the `rume_migrations` row immediately after `down_sql`, without
            // `begin_transaction()` or fake `START TRANSACTION`/`COMMIT`/`ROLLBACK` wrappers.
            #[cfg(feature = "mysql")]
            if let Some(mysql_backend) =
                (backend as &dyn std::any::Any).downcast_ref::<crate::persistence::MysqlBackend>()
            {
                let mut guard = MysqlMigrationLockGuard::acquire(mysql_backend.pool()).await?;
                let mut rolled_back = None;
                let run_res: Result<(), PersistenceError> = async {
                    let conn = guard.conn_mut();
                    mysql_conn_execute_raw(conn, ddl).await?;
                    for migration in self.migrations.iter().rev() {
                        let id_param = [DbValue::from(migration.id.as_str())];
                        let applied =
                            mysql_conn_query_scalar_params(conn, check_sql, &id_param).await?;
                        if applied.is_some() {
                            let down_sql = migration.down_sql.as_deref().ok_or_else(|| {
                                PersistenceError::IrreversibleMigration(migration.id.clone())
                            })?;
                            mysql_conn_execute_raw(conn, down_sql)
                                .await
                                .map_err(|err| PersistenceError::MigrationError {
                                    migration: migration.id.clone(),
                                    message: err.to_string(),
                                })?;
                            mysql_conn_execute_params(conn, delete_sql, &id_param).await?;
                            rolled_back = Some(migration.id.clone());
                            break;
                        }
                    }
                    Ok(())
                }
                .await;

                return match run_res {
                    Ok(()) => {
                        guard.release().await?;
                        Ok(rolled_back)
                    }
                    Err(err) => {
                        let _ = guard.release().await;
                        Err(err)
                    }
                };
            }

            backend.execute(ddl).await?;
            for migration in self.migrations.iter().rev() {
                let id_param = [DbValue::from(migration.id.as_str())];
                let exists = backend.query_scalar_params(check_sql, &id_param).await?;
                if exists.is_some() {
                    let down_sql = migration.down_sql.as_deref().ok_or_else(|| {
                        PersistenceError::IrreversibleMigration(migration.id.clone())
                    })?;
                    backend.execute(down_sql).await.map_err(|err| {
                        PersistenceError::MigrationError {
                            migration: migration.id.clone(),
                            message: err.to_string(),
                        }
                    })?;
                    backend.execute_params(delete_sql, &id_param).await?;
                    return Ok(Some(migration.id.clone()));
                }
            }
            return Ok(None);
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::persistence::backend::{BoxFuture, DatabaseTransaction};
    use crate::persistence::row::DbRow;

    #[derive(Debug, Clone)]
    struct MockMysqlTx;

    impl DatabaseTransaction for MockMysqlTx {
        fn dialect(&self) -> &'static str {
            "mysql"
        }

        fn execute_params(
            &self,
            _statement: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
            Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
        }

        fn query_scalar_params(
            &self,
            _query: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
            Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
        }

        fn fetch_all(
            &self,
            _statement: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
            Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
        }

        fn fetch_optional(
            &self,
            _statement: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
            Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
        }

        fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async {
                Err(PersistenceError::TransactionError(
                    "unexpected commit".into(),
                ))
            })
        }

        fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async {
                Err(PersistenceError::TransactionError(
                    "unexpected rollback".into(),
                ))
            })
        }
    }

    #[derive(Debug, Clone, Default)]
    struct MockMysqlBackend {
        executed_sql: Arc<Mutex<Vec<String>>>,
        ledger: Arc<Mutex<BTreeMap<String, String>>>,
        begin_tx_calls: Arc<Mutex<usize>>,
    }

    impl DatabaseBackend for MockMysqlBackend {
        type Transaction = MockMysqlTx;

        fn dialect(&self) -> &'static str {
            "mysql"
        }

        fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async { Ok(()) })
        }

        fn execute_params(
            &self,
            statement: &str,
            params: &[DbValue],
        ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
            let statement = statement.to_string();
            let params = params.to_vec();
            let executed_sql = Arc::clone(&self.executed_sql);
            let ledger = Arc::clone(&self.ledger);
            Box::pin(async move {
                executed_sql.lock().unwrap().push(statement.clone());
                if statement.contains("FAIL_MIGRATION") {
                    return Err(PersistenceError::QueryFailed("simulated failure".into()));
                }
                if statement == MYSQL_MIGRATION_INSERT {
                    if let (Some(DbValue::Text(id)), Some(DbValue::Text(checksum))) =
                        (params.first(), params.get(3))
                    {
                        ledger.lock().unwrap().insert(id.clone(), checksum.clone());
                    }
                } else if statement == MYSQL_MIGRATION_DELETE
                    && let Some(DbValue::Text(id)) = params.first()
                {
                    ledger.lock().unwrap().remove(id);
                }
                Ok(1)
            })
        }

        fn query_scalar_params(
            &self,
            query: &str,
            params: &[DbValue],
        ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
            let query = query.to_string();
            let params = params.to_vec();
            let executed_sql = Arc::clone(&self.executed_sql);
            let ledger = Arc::clone(&self.ledger);
            Box::pin(async move {
                executed_sql.lock().unwrap().push(query.clone());
                if query == MYSQL_MIGRATION_FIND_CHECKSUM {
                    if let Some(DbValue::Text(id)) = params.first() {
                        return Ok(ledger.lock().unwrap().get(id).cloned());
                    }
                } else if query == MYSQL_MIGRATION_FIND_ID {
                    if let Some(DbValue::Text(id)) = params.first() {
                        return Ok(ledger.lock().unwrap().contains_key(id).then(|| id.clone()));
                    }
                } else if query == MYSQL_CLOCK {
                    return Ok(Some("1700000000".to_string()));
                }
                Ok(None)
            })
        }

        fn fetch_all(
            &self,
            _statement: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn fetch_optional(
            &self,
            _statement: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
            Box::pin(async { Ok(None) })
        }

        fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
            let calls = Arc::clone(&self.begin_tx_calls);
            Box::pin(async move {
                *calls.lock().unwrap() += 1;
                Err(PersistenceError::TransactionError(
                    "MySQL migrations must not call begin_transaction()".into(),
                ))
            })
        }
    }

    #[test]
    fn core_down_scripts_never_drop_ledger_or_audit_tables() {
        for (dialect, sql) in [
            ("postgres", PG_CORE_DOWN),
            ("mysql", MYSQL_CORE_DOWN),
            ("sqlite", SQLITE_CORE_DOWN),
        ] {
            assert!(
                !sql.contains("rume_migrations"),
                "{dialect} core_down.sql must not drop rume_migrations"
            );
            assert!(
                !sql.contains("rume_audit_log"),
                "{dialect} core_down.sql must not drop rume_audit_log"
            );
        }
    }

    #[test]
    fn with_rume_core_registers_core_and_audit_ahead_of_app_migrations() {
        for dialect in ["postgres", "mysql", "sqlite"] {
            let migrator = Migrator::new()
                .add_migration(Migration::from_sql(
                    "20261105_001_app",
                    "App migration",
                    "CREATE TABLE app_items (id INT);",
                ))
                .with_rume_core(dialect);

            let migrations = migrator.migrations();
            assert_eq!(
                migrations.len(),
                3,
                "{dialect} must register 2 core migrations before app migrations"
            );
            assert_eq!(migrations[0].id, RUME_CORE_MIGRATION_ID);
            assert_eq!(migrations[1].id, RUME_AUDIT_MIGRATION_ID);
            assert_eq!(migrations[2].id, "20261105_001_app");
        }

        let unknown = Migrator::new().with_rume_core("unknown_dialect");
        assert!(unknown.migrations().is_empty());
    }

    #[tokio::test]
    async fn mysql_dialect_non_mysql_backend_mock_runs_and_rolls_back_without_transactions() {
        let backend = MockMysqlBackend::default();
        let migrator = Migrator::new()
            .add_migration(
                Migration::from_sql("001_first", "first", "CREATE TABLE t1 (id INT);")
                    .with_down("DROP TABLE t1;"),
            )
            .add_migration(Migration::from_sql(
                "002_fail",
                "second fails",
                "CREATE TABLE t2 (id INT); FAIL_MIGRATION;",
            ));

        let err = migrator
            .run_pending_backend(&backend)
            .await
            .expect_err("second migration must fail");
        assert!(matches!(
            err,
            PersistenceError::MigrationError { ref migration, .. } if migration == "002_fail"
        ));
        assert_eq!(*backend.begin_tx_calls.lock().unwrap(), 0);
        assert!(backend.ledger.lock().unwrap().contains_key("001_first"));
        assert!(!backend.ledger.lock().unwrap().contains_key("002_fail"));

        let executed = backend.executed_sql.lock().unwrap().clone();
        assert!(
            !executed
                .iter()
                .any(|s| { matches!(s.trim(), "START TRANSACTION" | "COMMIT" | "ROLLBACK") }),
            "MySQL migration runner must not execute START TRANSACTION/COMMIT/ROLLBACK: {executed:?}"
        );

        let rolled_back = migrator
            .rollback_last_backend(&backend)
            .await
            .expect("rollback of 001_first should succeed without begin_transaction");
        assert_eq!(rolled_back.as_deref(), Some("001_first"));
        assert_eq!(*backend.begin_tx_calls.lock().unwrap(), 0);
        assert!(!backend.ledger.lock().unwrap().contains_key("001_first"));
    }
}
