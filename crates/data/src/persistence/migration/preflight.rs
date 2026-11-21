//! Preflight and apply machinery for the migration runner: per-dialect SQL constants,
//! rollback-target selection, and the applied-order schema handling shared by every
//! backend.

use super::runner::Migration;
use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction};
use crate::persistence::error::PersistenceError;

#[cfg(feature = "mysql")]
use super::runner::{mysql_conn_execute_raw, mysql_conn_query_scalar_params};

pub(super) const PG_OUTBOX: &str = include_str!("../../../sql/postgres/outbox_schema.sql");
pub(super) const PG_JOBS: &str = include_str!("../../../sql/postgres/jobs_schema.sql");
pub(super) const PG_LOCKS: &str = include_str!("../../../sql/postgres/locks_schema.sql");
pub(super) const PG_LEADER: &str = include_str!("../../../sql/postgres/leader_schema.sql");
pub(super) const PG_AUDIT: &str = include_str!("../../../sql/postgres/audit_schema.sql");
pub(super) const PG_CORE_DOWN: &str = include_str!("../../../sql/postgres/core_down.sql");

pub(super) const MYSQL_OUTBOX: &str = include_str!("../../../sql/mysql/outbox_schema.sql");
pub(super) const MYSQL_AUDIT: &str = include_str!("../../../sql/mysql/audit_schema.sql");
pub(super) const MYSQL_CORE_DOWN: &str = include_str!("../../../sql/mysql/core_down.sql");

pub(super) const SQLITE_OUTBOX: &str = include_str!("../../../sql/sqlite/outbox_schema.sql");
pub(super) const SQLITE_AUDIT: &str = include_str!("../../../sql/sqlite/audit_schema.sql");
pub(super) const SQLITE_CORE_DOWN: &str = include_str!("../../../sql/sqlite/core_down.sql");

pub(super) const PG_MIGRATIONS_SCHEMA: &str =
    include_str!("../../../sql/postgres/migrations_schema.sql");
pub(super) const MYSQL_MIGRATIONS_SCHEMA: &str =
    include_str!("../../../sql/mysql/migrations_schema.sql");
pub(super) const SQLITE_MIGRATIONS_SCHEMA: &str =
    include_str!("../../../sql/sqlite/migrations_schema.sql");

/// Canonical DDL for the migration tracking ledger table across all database backends.
pub(super) const CREATE_MIGRATIONS_TABLE_DDL: &str = PG_MIGRATIONS_SCHEMA;

/// Returns the dialect-specific DDL for creating the `rume_migrations` table.
pub(super) fn migrations_schema_ddl(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATIONS_SCHEMA,
        "mysql" => MYSQL_MIGRATIONS_SCHEMA,
        "sqlite" => SQLITE_MIGRATIONS_SCHEMA,
        _ => CREATE_MIGRATIONS_TABLE_DDL,
    }
}

// Per-dialect migration query SQL, externalized from inline string literals.
pub(super) const PG_MIGRATION_FIND_CHECKSUM: &str =
    include_str!("../../../sql/postgres/migration_find_checksum.sql");
pub(super) const PG_MIGRATION_INSERT: &str =
    include_str!("../../../sql/postgres/migration_insert.sql");
pub(super) const PG_MIGRATION_DELETE: &str =
    include_str!("../../../sql/postgres/migration_delete.sql");
pub(super) const PG_MIGRATION_FIND_ID: &str =
    include_str!("../../../sql/postgres/migration_find_id.sql");
pub(super) const PG_MIGRATION_LIST_APPLIED: &str =
    include_str!("../../../sql/common/migration_list_applied.sql");
pub(super) const PG_ADVISORY_LOCK: &str =
    include_str!("../../../sql/postgres/pg_advisory_lock.sql");
pub(super) const PG_CLOCK: &str = include_str!("../../../sql/postgres/pg_clock.sql");

pub(super) const MYSQL_MIGRATION_FIND_CHECKSUM: &str =
    include_str!("../../../sql/mysql/migration_find_checksum.sql");
pub(super) const MYSQL_MIGRATION_INSERT: &str =
    include_str!("../../../sql/mysql/migration_insert.sql");
pub(super) const MYSQL_MIGRATION_DELETE: &str =
    include_str!("../../../sql/mysql/migration_delete.sql");
pub(super) const MYSQL_MIGRATION_FIND_ID: &str =
    include_str!("../../../sql/mysql/migration_find_id.sql");
#[cfg(any(feature = "mysql", test))]
pub(super) const MYSQL_CLOCK: &str = include_str!("../../../sql/mysql/mysql_clock.sql");
#[cfg(feature = "mysql")]
pub(super) const MYSQL_MIGRATION_LOCK: &str =
    include_str!("../../../sql/mysql/mysql_migration_lock.sql");
#[cfg(feature = "mysql")]
pub(super) const MYSQL_MIGRATION_UNLOCK: &str =
    include_str!("../../../sql/mysql/mysql_migration_unlock.sql");

pub(super) const SQLITE_MIGRATION_FIND_CHECKSUM: &str =
    include_str!("../../../sql/sqlite/migration_find_checksum.sql");
pub(super) const SQLITE_MIGRATION_INSERT: &str =
    include_str!("../../../sql/sqlite/migration_insert.sql");
pub(super) const SQLITE_MIGRATION_DELETE: &str =
    include_str!("../../../sql/sqlite/migration_delete.sql");
pub(super) const SQLITE_MIGRATION_FIND_ID: &str =
    include_str!("../../../sql/sqlite/migration_find_id.sql");
pub(super) const SQLITE_CLOCK: &str = include_str!("../../../sql/sqlite/sqlite_clock.sql");
pub(super) const SQLITE_BUSY_TIMEOUT: &str =
    include_str!("../../../sql/sqlite/sqlite_busy_timeout.sql");

pub(super) fn find_checksum_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_FIND_CHECKSUM,
        "mysql" => MYSQL_MIGRATION_FIND_CHECKSUM,
        _ => SQLITE_MIGRATION_FIND_CHECKSUM,
    }
}

pub(super) fn insert_migration_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_INSERT,
        "mysql" => MYSQL_MIGRATION_INSERT,
        _ => SQLITE_MIGRATION_INSERT,
    }
}

pub(super) fn delete_migration_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_DELETE,
        "mysql" => MYSQL_MIGRATION_DELETE,
        _ => SQLITE_MIGRATION_DELETE,
    }
}

pub(super) fn find_migration_id_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => PG_MIGRATION_FIND_ID,
        "mysql" => MYSQL_MIGRATION_FIND_ID,
        _ => SQLITE_MIGRATION_FIND_ID,
    }
}

pub(super) const LIST_APPLIED_MIGRATIONS_SQL: &str = PG_MIGRATION_LIST_APPLIED;
pub(super) const NEXT_APPLIED_ORDER_SQL: &str =
    "SELECT COALESCE(MAX(applied_order), 0) + 1 FROM rume_migrations";
#[cfg(feature = "mysql")]
pub(super) const APPLIED_ORDER_MYSQL_COUNT_SQL: &str = "SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'rume_migrations' AND COLUMN_NAME = 'applied_order'";
pub(super) const APPLIED_ORDER_SQLITE_COUNT_SQL: &str =
    "SELECT COUNT(*) FROM pragma_table_info('rume_migrations') WHERE name = 'applied_order'";
pub(super) const LIST_APPLIED_ORDER_SQL: &str = "SELECT id, applied_order, applied_at FROM rume_migrations ORDER BY applied_order DESC, applied_at DESC";

pub(super) fn select_rollback_target<'a>(
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

pub(super) async fn ensure_applied_order_column_tx<T: DatabaseTransaction>(
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
            // Why: `ensure_applied_order_column_tx` is only invoked from the `"postgres"` and
            // `"sqlite"` transactional paths; custom dialects use `ensure_applied_order_column_backend`.
        }
    }
    Ok(())
}

pub(super) async fn ensure_applied_order_column_backend<B: DatabaseBackend>(
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

pub(super) async fn next_applied_order_tx<T: DatabaseTransaction>(
    tx: &T,
) -> Result<i64, PersistenceError> {
    let raw = tx.query_scalar(NEXT_APPLIED_ORDER_SQL).await?;
    Ok(raw
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1))
}

pub(super) async fn fetch_applied_order_rows_tx<T: DatabaseTransaction>(
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
pub(super) async fn ensure_applied_order_column_mysql(
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
pub(super) async fn next_applied_order_mysql(
    conn: &mut sqlx::MySqlConnection,
) -> Result<i64, PersistenceError> {
    let raw = mysql_conn_query_scalar_params(conn, NEXT_APPLIED_ORDER_SQL, &[]).await?;
    Ok(raw
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1))
}

#[cfg(feature = "mysql")]
pub(super) async fn fetch_applied_order_rows_mysql(
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

pub(super) fn parse_clock_or_now(clock_res: Result<Option<String>, PersistenceError>) -> i64 {
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
