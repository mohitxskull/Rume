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
#[cfg(any(feature = "mysql", test))]
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
const NEXT_APPLIED_ORDER_SQL: &str =
    "SELECT COALESCE(MAX(applied_order), 0) + 1 FROM rume_migrations";
#[cfg(feature = "mysql")]
const APPLIED_ORDER_MYSQL_COUNT_SQL: &str = "SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'rume_migrations' AND COLUMN_NAME = 'applied_order'";
const APPLIED_ORDER_SQLITE_COUNT_SQL: &str =
    "SELECT COUNT(*) FROM pragma_table_info('rume_migrations') WHERE name = 'applied_order'";
const LIST_APPLIED_ORDER_SQL: &str = "SELECT id, applied_order, applied_at FROM rume_migrations ORDER BY applied_order DESC, applied_at DESC";

fn select_rollback_target<'a>(
    migrations: &'a [Migration],
    applied_rows: &[(String, i64, i64)],
) -> Option<&'a Migration> {
    let mut best: Option<(i64, i64, usize, &'a Migration)> = None;
    for (id, applied_order, applied_at) in applied_rows {
        if let Some((registry_idx, migration)) =
            migrations.iter().enumerate().find(|(_, m)| m.id == *id)
        {
            let key = (*applied_order, *applied_at, registry_idx);
            let replace = match best {
                None => true,
                Some((bo, ba, bi, _)) => key > (bo, ba, bi),
            };
            if replace {
                best = Some((*applied_order, *applied_at, registry_idx, migration));
            }
        }
    }
    best.map(|(_, _, _, m)| m)
}

async fn ensure_applied_order_column_tx<T: DatabaseTransaction>(
    tx: &T,
    dialect: &str,
) -> Result<(), PersistenceError> {
    match dialect {
        "postgres" => {
            tx.execute(
                "ALTER TABLE rume_migrations ADD COLUMN IF NOT EXISTS applied_order BIGINT NOT NULL DEFAULT 0",
            )
            .await?;
        }
        "sqlite" => {
            let count = tx.query_scalar(APPLIED_ORDER_SQLITE_COUNT_SQL).await?;
            if count.as_deref() == Some("0") {
                let alter = tx
                    .execute(
                        "ALTER TABLE rume_migrations ADD COLUMN applied_order INTEGER NOT NULL DEFAULT 0",
                    )
                    .await;
                if let Err(err) = alter {
                    // Why: tolerate a concurrent ADD COLUMN only after re-verifying the column.
                    let recount = tx.query_scalar(APPLIED_ORDER_SQLITE_COUNT_SQL).await?;
                    if recount.as_deref() == Some("0") {
                        return Err(err);
                    }
                }
            }
        }
        _ => {
            // Why: Custom `DatabaseBackend` dialects have no dialect-specific catalog query, so
            // probe `applied_order` directly and run standard SQL `ALTER TABLE ... ADD COLUMN`
            // only if the probe fails (re-probing after any concurrent `ALTER` race).
            if tx
                .query_scalar("SELECT applied_order FROM rume_migrations WHERE 1 = 0")
                .await
                .is_err()
            {
                let alter = tx
                    .execute(
                        "ALTER TABLE rume_migrations ADD COLUMN applied_order BIGINT NOT NULL DEFAULT 0",
                    )
                    .await;
                if let Err(err) = alter {
                    tx.query_scalar("SELECT applied_order FROM rume_migrations WHERE 1 = 0")
                        .await
                        .map_err(|_| err)?;
                }
            }
        }
    }
    Ok(())
}

async fn ensure_applied_order_column_backend<B: DatabaseBackend>(
    backend: &B,
) -> Result<(), PersistenceError> {
    match backend.dialect() {
        "postgres" => {
            backend
                .execute(
                    "ALTER TABLE rume_migrations ADD COLUMN IF NOT EXISTS applied_order BIGINT NOT NULL DEFAULT 0",
                )
                .await?;
        }
        "sqlite" => {
            let count = backend.query_scalar(APPLIED_ORDER_SQLITE_COUNT_SQL).await?;
            if count.as_deref() == Some("0") {
                let alter = backend
                    .execute(
                        "ALTER TABLE rume_migrations ADD COLUMN applied_order INTEGER NOT NULL DEFAULT 0",
                    )
                    .await;
                if let Err(err) = alter {
                    // Why: `applied_migrations` runs outside the migration lock, so a concurrent
                    // migrator may add the column first; tolerate only after re-verifying it.
                    let recount = backend.query_scalar(APPLIED_ORDER_SQLITE_COUNT_SQL).await?;
                    if recount.as_deref() == Some("0") {
                        return Err(err);
                    }
                }
            }
        }
        "mysql" => {
            let count = backend
                .query_scalar(
                    "SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'rume_migrations' AND COLUMN_NAME = 'applied_order'",
                )
                .await?;
            if count.as_deref() == Some("0") {
                let alter = backend
                    .execute(
                        "ALTER TABLE rume_migrations ADD COLUMN applied_order BIGINT NOT NULL DEFAULT 0",
                    )
                    .await;
                if let Err(err) = alter {
                    // Why: `applied_migrations` runs outside the named lock, so a concurrent
                    // migrator may add the column first (duplicate-column error); tolerate only
                    // after re-verifying the column exists.
                    let recount = backend
                        .query_scalar(
                            "SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'rume_migrations' AND COLUMN_NAME = 'applied_order'",
                        )
                        .await?;
                    if recount.as_deref() == Some("0") {
                        return Err(err);
                    }
                }
            }
        }
        _ => {
            // Why: Custom `DatabaseBackend` dialects have no dialect-specific catalog query, so
            // probe `applied_order` directly and run standard SQL `ALTER TABLE ... ADD COLUMN`
            // only if the probe fails (re-probing after any concurrent `ALTER` race).
            if backend
                .query_scalar("SELECT applied_order FROM rume_migrations WHERE 1 = 0")
                .await
                .is_err()
            {
                let alter = backend
                    .execute(
                        "ALTER TABLE rume_migrations ADD COLUMN applied_order BIGINT NOT NULL DEFAULT 0",
                    )
                    .await;
                if let Err(err) = alter {
                    backend
                        .query_scalar("SELECT applied_order FROM rume_migrations WHERE 1 = 0")
                        .await
                        .map_err(|_| err)?;
                }
            }
        }
    }
    Ok(())
}

async fn next_applied_order_tx<T: DatabaseTransaction>(tx: &T) -> Result<i64, PersistenceError> {
    let raw = tx.query_scalar(NEXT_APPLIED_ORDER_SQL).await?;
    Ok(raw
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1))
}

async fn fetch_applied_order_rows_tx<T: DatabaseTransaction>(
    tx: &T,
) -> Result<Vec<(String, i64, i64)>, PersistenceError> {
    let rows = tx.fetch_all(LIST_APPLIED_ORDER_SQL, &[]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get("id")?;
        let applied_order: i64 = row.get("applied_order")?;
        let applied_at: i64 = row.get("applied_at")?;
        out.push((id, applied_order, applied_at));
    }
    Ok(out)
}

#[cfg(feature = "mysql")]
async fn ensure_applied_order_column_mysql(
    conn: &mut sqlx::MySqlConnection,
) -> Result<(), PersistenceError> {
    let count = mysql_conn_query_scalar_params(conn, APPLIED_ORDER_MYSQL_COUNT_SQL, &[]).await?;
    if count.as_deref() == Some("0") {
        let alter = mysql_conn_execute_raw(
            conn,
            "ALTER TABLE rume_migrations ADD COLUMN applied_order BIGINT NOT NULL DEFAULT 0",
        )
        .await;
        if let Err(err) = alter {
            // Why: a concurrent migrator may have added the column between the check and the
            // ALTER; tolerate the duplicate-column failure only after re-verifying the column.
            let recount =
                mysql_conn_query_scalar_params(conn, APPLIED_ORDER_MYSQL_COUNT_SQL, &[]).await?;
            if recount.as_deref() == Some("0") {
                return Err(err);
            }
        }
    }
    Ok(())
}

#[cfg(feature = "mysql")]
async fn next_applied_order_mysql(
    conn: &mut sqlx::MySqlConnection,
) -> Result<i64, PersistenceError> {
    let raw = mysql_conn_query_scalar_params(conn, NEXT_APPLIED_ORDER_SQL, &[]).await?;
    Ok(raw
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1))
}

#[cfg(feature = "mysql")]
async fn fetch_applied_order_rows_mysql(
    conn: &mut sqlx::MySqlConnection,
) -> Result<Vec<(String, i64, i64)>, PersistenceError> {
    use sqlx::Row;
    let rows = sqlx::query(LIST_APPLIED_ORDER_SQL)
        .fetch_all(conn)
        .await
        .map_err(crate::persistence::backend::map_sqlx_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row
            .try_get("id")
            .map_err(|e| PersistenceError::DecodeError {
                column: "id".into(),
                message: e.to_string(),
            })?;
        let applied_order: i64 =
            row.try_get("applied_order")
                .map_err(|e| PersistenceError::DecodeError {
                    column: "applied_order".into(),
                    message: e.to_string(),
                })?;
        let applied_at: i64 = row
            .try_get::<i64, _>("applied_at")
            .or_else(|_| row.try_get::<u64, _>("applied_at").map(|v| v as i64))
            .map_err(|e| PersistenceError::DecodeError {
                column: "applied_at".into(),
                message: e.to_string(),
            })?;
        out.push((id, applied_order, applied_at));
    }
    Ok(out)
}

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

/// Rolls back any transaction still open on the pinned session, then releases the named lock.
///
/// Why: a cancelled migration (or a failed explicit `ROLLBACK`, whose result the migration loops
/// discard) can leave `START TRANSACTION` open. SQLx only `ping()`s a connection before reusing
/// it from the pool, so the next borrower would run inside that open transaction, see
/// uncommitted migration rows, and could `COMMIT` them without their ledger row. `ROLLBACK` with
/// no open transaction is a no-op in MySQL, so issuing it unconditionally needs no extra state
/// tracking. If `ROLLBACK` fails, `RELEASE_LOCK` is skipped and the error is returned so the
/// caller detaches the session (MySQL then discards the transaction and drops the lock itself).
/// Revisit if the pool gains an `after_release` hook that resets session state.
#[cfg(feature = "mysql")]
async fn mysql_rollback_then_unlock(
    conn: &mut sqlx::MySqlConnection,
) -> Result<Option<String>, PersistenceError> {
    mysql_conn_execute_raw(conn, "ROLLBACK").await?;
    mysql_conn_query_scalar_params(conn, MYSQL_MIGRATION_UNLOCK, &[]).await
}

/// Acquisition / ownership state of [`MysqlMigrationLockGuard`].
#[cfg(feature = "mysql")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum MysqlLockState {
    /// `GET_LOCK` is in-flight or returned an indeterminate transport error; the session may
    /// hold `rume_migrations_lock` on the server, so dropping in this state MUST detach and
    /// close the physical connection rather than returning it to the pool.
    Acquiring,
    /// `GET_LOCK` returned `"1"`; session definitely holds `rume_migrations_lock`.
    Locked,
    /// `RELEASE_LOCK` returned `"1"` or `GET_LOCK` definitively returned non-`"1"`.
    Released,
}

/// RAII guard owning a pinned MySQL pool connection while `rume_migrations_lock` is held.
///
/// Why: MySQL `GET_LOCK`/`RELEASE_LOCK` are session-bound (`CONNECTION_ID()`), and MySQL DDL
/// implicitly auto-commits every statement. Pinning a single `PoolConnection<MySql>` directly via
/// `DatabaseBackend::mysql_migration_pool()` keeps lock acquisition, per-migration execution
/// (transactional for pure InnoDB DML, direct for implicit-commit DDL), and lock release on the
/// same physical session.
#[cfg(feature = "mysql")]
struct MysqlMigrationLockGuard {
    conn: Option<sqlx::pool::PoolConnection<sqlx::MySql>>,
    state: MysqlLockState,
}

#[cfg(feature = "mysql")]
impl MysqlMigrationLockGuard {
    async fn acquire(pool: &sqlx::MySqlPool) -> Result<Self, PersistenceError> {
        let conn = pool
            .acquire()
            .await
            .map_err(crate::persistence::backend::map_sqlx_error)?;
        // Why: mark `Acquiring` BEFORE awaiting `GET_LOCK` so cancelling the future after
        // MySQL receives `GET_LOCK` (or hitting a transport error) detaches the physical
        // connection in `Drop` instead of returning a potentially-locked session to the pool.
        let mut guard = Self {
            conn: Some(conn),
            state: MysqlLockState::Acquiring,
        };
        let lock_res =
            mysql_conn_query_scalar_params(guard.conn_mut()?, MYSQL_MIGRATION_LOCK, &[]).await?;
        if lock_res.as_deref() != Some("1") {
            // Definitive non-acquisition (`0` or `NULL`): session does not hold the lock,
            // so it is safe to return the connection to the pool.
            guard.state = MysqlLockState::Released;
            return Err(PersistenceError::Timeout);
        }
        guard.state = MysqlLockState::Locked;
        Ok(guard)
    }

    fn conn_mut(&mut self) -> Result<&mut sqlx::MySqlConnection, PersistenceError> {
        self.conn.as_deref_mut().ok_or_else(|| {
            PersistenceError::TransactionError("MySQL migration connection already released".into())
        })
    }

    async fn release(mut self) -> Result<(), PersistenceError> {
        if self.state != MysqlLockState::Locked {
            return Ok(());
        }
        let unlock_res = mysql_rollback_then_unlock(self.conn_mut()?).await;
        // Why: disarm only after awaiting cleanup so cancellation mid-unlock still triggers
        // `Drop` cleanup. Only return the connection to the pool when `ROLLBACK` succeeded AND
        // `RELEASE_LOCK` affirmatively returned `"1"`; otherwise detach and close the session so
        // MySQL discards any open transaction and drops any named lock it still holds.
        self.state = MysqlLockState::Released;
        let released = matches!(&unlock_res, Ok(Some(s)) if s == "1");
        if released {
            let _ = self.conn.take();
        } else if let Some(conn) = self.conn.take() {
            drop(conn.detach());
        }

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
        match self.state {
            MysqlLockState::Released => {}
            MysqlLockState::Acquiring => {
                if let Some(conn) = self.conn.take() {
                    // Acquisition outcome is unknown (cancelled or transport error); close the
                    // physical session so MySQL releases any lock it may have granted.
                    drop(conn.detach());
                }
            }
            MysqlLockState::Locked => {
                if let Some(mut conn) = self.conn.take() {
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        // Why: Cancellation-safe cleanup matching `MysqlTransaction::drop`; if
                        // `RELEASE_LOCK` does not affirmatively return `"1"`, detach the
                        // connection so MySQL closes the session and drops the named lock.
                        handle.spawn(async move {
                            let res = mysql_rollback_then_unlock(&mut conn).await;
                            if !matches!(res, Ok(Some(ref s)) if s == "1") {
                                drop(conn.detach());
                            }
                        });
                    } else {
                        drop(conn.detach());
                    }
                }
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
        // Why: `RUME_AUDIT_MIGRATION_ID` uses `CREATE TABLE IF NOT EXISTS rume_audit_log` so
        // databases upgraded from `SqlAuditLogger::init_table()` succeed without error. Because
        // the runner cannot distinguish a pre-existing populated audit table from a freshly
        // created one, this migration is intentionally irreversible (`down_sql: None`) so
        // rolling back never drops pre-existing audit history.
        self.migrations.insert(
            1,
            Migration::from_sql(
                RUME_AUDIT_MIGRATION_ID,
                "Initialize Rume audit log table",
                audit_up_sql,
            ),
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
        ensure_applied_order_column_backend(backend).await?;
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
            ensure_applied_order_column_tx(&tx, dialect).await?;

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
                    let now = parse_clock_or_now(tx.query_scalar(PG_CLOCK).await);
                    let next_order = next_applied_order_tx(&tx).await?;
                    let params = [
                        DbValue::from(migration.id.as_str()),
                        DbValue::from(migration.description.as_str()),
                        DbValue::Int(now),
                        DbValue::from(migration.checksum().as_str()),
                        DbValue::Int(next_order),
                    ];
                    tx.execute_params(insert_sql, &params).await?;
                    applied.push(migration.id.clone());
                }
            }
            tx.commit().await?;
            return Ok(MigrationSummary { applied });
        }

        if dialect == "mysql" {
            // Why: Acquire the database-wide `rume_migrations_lock` on a pinned session BEFORE
            // reading or mutating `rume_migrations`. If a MySQL backend (or wrapper) does not
            // expose `mysql_migration_pool()`, fail closed before mutating the database rather
            // than silently running unlocked. For each pending migration, pure InnoDB DML runs
            // atomically with its ledger insert inside an explicit transaction on the pinned
            // connection, while DDL (which implicitly commits in MySQL) runs directly followed
            // by an immediate ledger insert.
            #[cfg(feature = "mysql")]
            {
                let pool = backend.mysql_migration_pool().ok_or_else(|| {
                    PersistenceError::TransactionError(
                        "MySQL migrations require a session-pinned migration lock capability (mysql_migration_pool)"
                            .into(),
                    )
                })?;
                let mut guard = MysqlMigrationLockGuard::acquire(pool).await?;
                let mut applied = Vec::new();
                let run_res: Result<(), PersistenceError> = async {
                    let conn = guard.conn_mut()?;
                    mysql_conn_execute_raw(conn, ddl).await?;
                    ensure_applied_order_column_mysql(conn).await?;
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
                            let is_transactional_dml = match &migration.up_sql {
                                Some(sql) => {
                                    !crate::persistence::backend::mysql_sql_causes_implicit_commit(
                                        sql,
                                    )?
                                }
                                None => true,
                            };
                            if is_transactional_dml {
                                mysql_conn_execute_raw(conn, "START TRANSACTION").await?;
                                let dml_res: Result<(), PersistenceError> = async {
                                    if let Some(sql) = &migration.up_sql {
                                        mysql_conn_execute_raw(conn, sql).await.map_err(|err| {
                                            PersistenceError::MigrationError {
                                                migration: migration.id.clone(),
                                                message: err.to_string(),
                                            }
                                        })?;
                                    }
                                    let now = parse_clock_or_now(
                                        mysql_conn_query_scalar_params(conn, MYSQL_CLOCK, &[])
                                            .await,
                                    );
                                    let next_order = next_applied_order_mysql(conn).await?;
                                    let params = [
                                        DbValue::from(migration.id.as_str()),
                                        DbValue::from(migration.description.as_str()),
                                        DbValue::Int(now),
                                        DbValue::from(migration.checksum().as_str()),
                                        DbValue::Int(next_order),
                                    ];
                                    mysql_conn_execute_params(conn, insert_sql, &params).await?;
                                    Ok(())
                                }
                                .await;
                                match dml_res {
                                    Ok(()) => {
                                        mysql_conn_execute_raw(conn, "COMMIT").await?;
                                    }
                                    Err(err) => {
                                        let _ = mysql_conn_execute_raw(conn, "ROLLBACK").await;
                                        return Err(err);
                                    }
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
                                let next_order = next_applied_order_mysql(conn).await?;
                                let params = [
                                    DbValue::from(migration.id.as_str()),
                                    DbValue::from(migration.description.as_str()),
                                    DbValue::Int(now),
                                    DbValue::from(migration.checksum().as_str()),
                                    DbValue::Int(next_order),
                                ];
                                mysql_conn_execute_params(conn, insert_sql, &params).await?;
                            }
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

            #[cfg(not(feature = "mysql"))]
            return Err(PersistenceError::TransactionError(
                "MySQL migrations require the `mysql` feature for session-pinned locking".into(),
            ));
        }

        if dialect == "sqlite" {
            let tx = backend.begin_transaction().await?;
            // Set PRAGMA busy_timeout on the exact transaction connection rather than an arbitrary pooled connection.
            tx.execute(SQLITE_BUSY_TIMEOUT).await?;
            tx.execute(ddl).await?;
            ensure_applied_order_column_tx(&tx, dialect).await?;

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
                    let now = parse_clock_or_now(tx.query_scalar(SQLITE_CLOCK).await);
                    let next_order = next_applied_order_tx(&tx).await?;
                    let params = [
                        DbValue::from(migration.id.as_str()),
                        DbValue::from(migration.description.as_str()),
                        DbValue::Int(now),
                        DbValue::from(migration.checksum().as_str()),
                        DbValue::Int(next_order),
                    ];
                    tx.execute_params(insert_sql, &params).await?;
                    applied.push(migration.id.clone());
                }
            }
            tx.commit().await?;
            return Ok(MigrationSummary { applied });
        }

        backend.execute(ddl).await?;
        ensure_applied_order_column_backend(backend).await?;

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
                // Why: `postgres`, `mysql`, and `sqlite` all return early above; this fallback
                // serves custom `DatabaseBackend` dialects, which record system wall-clock time.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let next_order = next_applied_order_tx(&tx).await?;
                let params = [
                    DbValue::from(migration.id.as_str()),
                    DbValue::from(migration.description.as_str()),
                    DbValue::Int(now),
                    DbValue::from(migration.checksum().as_str()),
                    DbValue::Int(next_order),
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
            ensure_applied_order_column_tx(&tx, dialect).await?;

            let applied_rows = fetch_applied_order_rows_tx(&tx).await?;
            if let Some(migration) = select_rollback_target(&self.migrations, &applied_rows) {
                let down_sql = migration
                    .down_sql
                    .as_deref()
                    .ok_or_else(|| PersistenceError::IrreversibleMigration(migration.id.clone()))?;
                let id_param = [DbValue::from(migration.id.as_str())];
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
            tx.commit().await?;
            return Ok(None);
        }

        if dialect == "mysql" {
            // Why: See `run_pending_backend` MySQL branch — require session-pinned `GET_LOCK`
            // via `mysql_migration_pool()` before reading or mutating `rume_migrations`, wrap
            // pure InnoDB DML `down_sql` + ledger delete in a transaction, and run DDL directly.
            #[cfg(feature = "mysql")]
            {
                let pool = backend.mysql_migration_pool().ok_or_else(|| {
                    PersistenceError::TransactionError(
                        "MySQL migrations require a session-pinned migration lock capability (mysql_migration_pool)"
                            .into(),
                    )
                })?;
                let mut guard = MysqlMigrationLockGuard::acquire(pool).await?;
                let mut rolled_back = None;
                let run_res: Result<(), PersistenceError> = async {
                    let conn = guard.conn_mut()?;
                    mysql_conn_execute_raw(conn, ddl).await?;
                    ensure_applied_order_column_mysql(conn).await?;
                    let applied_rows = fetch_applied_order_rows_mysql(conn).await?;
                    if let Some(migration) = select_rollback_target(&self.migrations, &applied_rows)
                    {
                        let down_sql = migration.down_sql.as_deref().ok_or_else(|| {
                            PersistenceError::IrreversibleMigration(migration.id.clone())
                        })?;
                        let id_param = [DbValue::from(migration.id.as_str())];
                        let is_transactional_dml =
                            !crate::persistence::backend::mysql_sql_causes_implicit_commit(
                                down_sql,
                            )?;
                        if is_transactional_dml {
                            mysql_conn_execute_raw(conn, "START TRANSACTION").await?;
                            let dml_res: Result<(), PersistenceError> = async {
                                mysql_conn_execute_raw(conn, down_sql)
                                    .await
                                    .map_err(|err| PersistenceError::MigrationError {
                                        migration: migration.id.clone(),
                                        message: err.to_string(),
                                    })?;
                                mysql_conn_execute_params(conn, delete_sql, &id_param).await?;
                                Ok(())
                            }
                            .await;
                            match dml_res {
                                Ok(()) => {
                                    mysql_conn_execute_raw(conn, "COMMIT").await?;
                                }
                                Err(err) => {
                                    let _ = mysql_conn_execute_raw(conn, "ROLLBACK").await;
                                    return Err(err);
                                }
                            }
                        } else {
                            mysql_conn_execute_raw(conn, down_sql)
                                .await
                                .map_err(|err| PersistenceError::MigrationError {
                                    migration: migration.id.clone(),
                                    message: err.to_string(),
                                })?;
                            mysql_conn_execute_params(conn, delete_sql, &id_param).await?;
                        }
                        rolled_back = Some(migration.id.clone());
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

            #[cfg(not(feature = "mysql"))]
            return Err(PersistenceError::TransactionError(
                "MySQL migrations require the `mysql` feature for session-pinned locking".into(),
            ));
        }

        if dialect == "sqlite" {
            let tx = backend.begin_transaction().await?;
            // Set PRAGMA busy_timeout on the exact transaction connection rather than an arbitrary pooled connection.
            tx.execute(SQLITE_BUSY_TIMEOUT).await?;
            tx.execute(ddl).await?;
            ensure_applied_order_column_tx(&tx, dialect).await?;

            let applied_rows = fetch_applied_order_rows_tx(&tx).await?;
            if let Some(migration) = select_rollback_target(&self.migrations, &applied_rows) {
                let down_sql = migration
                    .down_sql
                    .as_deref()
                    .ok_or_else(|| PersistenceError::IrreversibleMigration(migration.id.clone()))?;
                let id_param = [DbValue::from(migration.id.as_str())];
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
            assert!(
                migrations[1].down_sql.is_none(),
                "{dialect} audit migration must be irreversible to protect pre-existing audit history"
            );
            assert_eq!(migrations[2].id, "20261105_001_app");
        }

        let unknown = Migrator::new().with_rume_core("unknown_dialect");
        assert!(unknown.migrations().is_empty());
    }

    #[tokio::test]
    async fn mysql_dialect_without_migration_lock_capability_fails_closed_before_mutation() {
        let backend = MockMysqlBackend::default();
        let migrator = Migrator::new().add_migration(
            Migration::from_sql("001_first", "first", "CREATE TABLE t1 (id INT);")
                .with_down("DROP TABLE t1;"),
        );

        let run_err = migrator
            .run_pending_backend(&backend)
            .await
            .expect_err("MySQL backend without migration lock capability must fail closed");
        assert!(matches!(run_err, PersistenceError::TransactionError(_)));
        assert!(
            backend.executed_sql.lock().unwrap().is_empty(),
            "no SQL must execute before acquiring MySQL migration lock"
        );
        assert!(backend.ledger.lock().unwrap().is_empty());

        let rb_err = migrator
            .rollback_last_backend(&backend)
            .await
            .expect_err("MySQL rollback without migration lock capability must fail closed");
        assert!(matches!(rb_err, PersistenceError::TransactionError(_)));
        assert!(
            backend.executed_sql.lock().unwrap().is_empty(),
            "no SQL must execute during unlocked MySQL rollback"
        );
    }

    #[test]
    fn select_rollback_target_prefers_actual_application_order_over_registry_order() {
        let migrations = vec![
            Migration::from_sql("core", "core", "SELECT 1;"),
            Migration::from_sql("audit", "audit", "SELECT 1;"),
            Migration::from_sql("app_a", "app_a", "SELECT 1;"),
        ];

        // Upgraded DB: core (order 1) and app_a (order 2) applied first, then audit (order 3)
        // applied in the same second. Even though `audit` precedes `app_a` in `migrations`,
        // actual application order selects `audit`.
        let applied = vec![
            ("audit".to_string(), 3, 1700000000),
            ("app_a".to_string(), 2, 1700000000),
            ("core".to_string(), 1, 1700000000),
        ];
        let target = select_rollback_target(&migrations, &applied).expect("target present");
        assert_eq!(target.id, "audit");
    }

    #[derive(Debug, Clone, Default)]
    struct MockGenericTx {
        executed_sql: Arc<Mutex<Vec<String>>>,
        ledger: Arc<Mutex<BTreeMap<String, String>>>,
        has_applied_order: Arc<Mutex<bool>>,
    }

    impl DatabaseTransaction for MockGenericTx {
        fn dialect(&self) -> &'static str {
            "custom_sql"
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
            let has_applied_order = Arc::clone(&self.has_applied_order);
            Box::pin(async move {
                executed_sql.lock().unwrap().push(statement.clone());
                if statement == SQLITE_MIGRATION_INSERT {
                    if !*has_applied_order.lock().unwrap() {
                        return Err(PersistenceError::QueryFailed(
                            "column applied_order does not exist".into(),
                        ));
                    }
                    if let (Some(DbValue::Text(id)), Some(DbValue::Text(checksum))) =
                        (params.first(), params.get(3))
                    {
                        ledger.lock().unwrap().insert(id.clone(), checksum.clone());
                    }
                }
                Ok(1)
            })
        }

        fn query_scalar_params(
            &self,
            query: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
            let query = query.to_string();
            let executed_sql = Arc::clone(&self.executed_sql);
            let has_applied_order = Arc::clone(&self.has_applied_order);
            Box::pin(async move {
                executed_sql.lock().unwrap().push(query.clone());
                if query.contains("applied_order") && !*has_applied_order.lock().unwrap() {
                    return Err(PersistenceError::QueryFailed(
                        "column applied_order does not exist".into(),
                    ));
                }
                Ok(Some("1".to_string()))
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

        fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Debug, Clone, Default)]
    struct MockGenericBackend {
        executed_sql: Arc<Mutex<Vec<String>>>,
        ledger: Arc<Mutex<BTreeMap<String, String>>>,
        has_applied_order: Arc<Mutex<bool>>,
    }

    impl DatabaseBackend for MockGenericBackend {
        type Transaction = MockGenericTx;

        fn dialect(&self) -> &'static str {
            "custom_sql"
        }

        fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async { Ok(()) })
        }

        fn execute_params(
            &self,
            statement: &str,
            _params: &[DbValue],
        ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
            let statement = statement.to_string();
            let executed_sql = Arc::clone(&self.executed_sql);
            let has_applied_order = Arc::clone(&self.has_applied_order);
            Box::pin(async move {
                executed_sql.lock().unwrap().push(statement.clone());
                if statement.starts_with("ALTER TABLE rume_migrations ADD COLUMN")
                    && statement.contains("applied_order")
                {
                    let mut col = has_applied_order.lock().unwrap();
                    if *col {
                        return Err(PersistenceError::QueryFailed(
                            "duplicate column applied_order".into(),
                        ));
                    }
                    *col = true;
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
            let has_applied_order = Arc::clone(&self.has_applied_order);
            Box::pin(async move {
                executed_sql.lock().unwrap().push(query.clone());
                if query.contains("applied_order") && !*has_applied_order.lock().unwrap() {
                    return Err(PersistenceError::QueryFailed(
                        "column applied_order does not exist".into(),
                    ));
                }
                if query == SQLITE_MIGRATION_FIND_CHECKSUM
                    && let Some(DbValue::Text(id)) = params.first()
                {
                    return Ok(ledger.lock().unwrap().get(id).cloned());
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
            let tx = MockGenericTx {
                executed_sql: Arc::clone(&self.executed_sql),
                ledger: Arc::clone(&self.ledger),
                has_applied_order: Arc::clone(&self.has_applied_order),
            };
            Box::pin(async move { Ok(tx) })
        }
    }

    #[tokio::test]
    async fn generic_backend_fallback_ensures_applied_order_column_before_insert() {
        // Start with a legacy ledger table lacking `applied_order` (`has_applied_order = false`).
        let backend = MockGenericBackend::default();
        let migrator = Migrator::new().add_migration(Migration::from_sql(
            "001_custom",
            "custom migration",
            "CREATE TABLE t_custom (id INT);",
        ));

        let summary = migrator
            .run_pending_backend(&backend)
            .await
            .expect("generic fallback migration upgrades legacy table and succeeds");
        assert_eq!(summary.applied.len(), 1);
        assert!(
            *backend.has_applied_order.lock().unwrap(),
            "generic fallback must add applied_order column when missing"
        );

        let log = backend.executed_sql.lock().unwrap().clone();
        let alter_idx = log
            .iter()
            .position(|s| {
                s.starts_with("ALTER TABLE rume_migrations ADD COLUMN")
                    && s.contains("applied_order")
            })
            .expect("ALTER TABLE ... ADD COLUMN applied_order must run on legacy ledger");
        let max_order_idx = log
            .iter()
            .position(|s| s == NEXT_APPLIED_ORDER_SQL)
            .expect("next_applied_order_tx must run");
        let insert_idx = log
            .iter()
            .position(|s| s == SQLITE_MIGRATION_INSERT)
            .expect("migration insert must run");
        assert!(
            alter_idx < max_order_idx && max_order_idx < insert_idx,
            "column setup must precede order-dependent queries and inserts: {log:?}"
        );

        // Second run when `applied_order` already exists must succeed without re-running ALTER.
        let summary2 = migrator
            .run_pending_backend(&backend)
            .await
            .expect("second run with existing applied_order column succeeds");
        assert!(summary2.applied.is_empty());
    }
}
