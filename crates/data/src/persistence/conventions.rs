//! Soft deletes and optimistic locking conventions.

use sea_query::{Alias, Expr, ExprTrait, IntoColumnRef, IntoIden, IntoTableRef, Query};

use crate::persistence::backend::{DatabaseBackend, DatabaseTransaction};
use crate::persistence::builder::{BuiltQuery, DynamicSelect, DynamicUpdate};
use crate::persistence::error::PersistenceError;
use crate::persistence::pool::DatabasePool;

/// Default column name used by [`SoftDeletable`] entities.
pub const DELETED_AT_COLUMN: &str = "deleted_at";

/// Default column name used by [`OptimisticLock`] entities.
pub const VERSION_COLUMN: &str = "version";

/// Convention trait for entities that support soft deletion via a nullable timestamp column.
pub trait SoftDeletable {
    /// Column name storing the soft-delete timestamp.
    const DELETED_AT_COLUMN: &'static str = DELETED_AT_COLUMN;

    /// Returns the soft-delete timestamp if the entity has been deleted.
    fn deleted_at(&self) -> Option<&str>;

    /// Returns `true` if the entity is currently soft-deleted.
    fn is_deleted(&self) -> bool {
        self.deleted_at().is_some()
    }
}

/// Adds `AND <col> IS NULL` to a [`DynamicSelect`] to exclude soft-deleted rows.
pub fn where_not_deleted(
    mut select: DynamicSelect,
    table_or_col: impl IntoColumnRef,
) -> DynamicSelect {
    select
        .as_inner_mut()
        .and_where(Expr::col(table_or_col).is_null());
    select
}

/// Adds `AND <col> IS NOT NULL` to a [`DynamicSelect`] to select only soft-deleted rows.
pub fn where_only_deleted(
    mut select: DynamicSelect,
    table_or_col: impl IntoColumnRef,
) -> DynamicSelect {
    select
        .as_inner_mut()
        .and_where(Expr::col(table_or_col).is_not_null());
    select
}

/// Builds a [`DynamicUpdate`] performing `UPDATE <table> SET deleted_at = ? WHERE deleted_at IS NULL`.
pub fn soft_delete_update(table: impl IntoTableRef, timestamp: impl Into<String>) -> DynamicUpdate {
    let col = Alias::new(DELETED_AT_COLUMN);
    let mut stmt = Query::update();
    stmt.table(table)
        .value(col.clone(), timestamp.into())
        .and_where(Expr::col(col).is_null());
    DynamicUpdate::new(stmt)
}

/// Convention trait for entities protected by optimistic concurrency control.
pub trait OptimisticLock {
    /// Column name storing the monotonically increasing version number.
    const VERSION_COLUMN: &'static str = VERSION_COLUMN;

    /// Returns the entity's current version number.
    fn version(&self) -> i64;
}

/// Adds `SET <col> = <col> + 1` and `AND <col> = <expected_version>` to a [`DynamicUpdate`]
/// for compare-and-swap optimistic locking.
pub fn with_version_check(
    mut update: DynamicUpdate,
    col: impl IntoIden + Clone,
    expected_version: i64,
) -> DynamicUpdate {
    let col_iden = col.into_iden();
    update
        .as_inner_mut()
        .value(col_iden.clone(), Expr::col(col_iden.clone()).add(1_i64))
        .and_where(Expr::col(col_iden).eq(expected_version));
    update
}

/// Executes a compare-and-swap [`BuiltQuery`] against `pool`, returning
/// [`PersistenceError::VersionConflict`] if zero rows were affected.
pub async fn execute_cas<B: DatabaseBackend>(
    pool: &DatabasePool<B>,
    built_query: &BuiltQuery,
    entity: impl Into<String>,
    expected_version: i64,
) -> Result<u64, PersistenceError> {
    let affected = built_query.execute(pool).await?;
    if affected == 0 {
        return Err(PersistenceError::VersionConflict {
            entity: entity.into(),
            expected: expected_version,
            actual: None,
        });
    }
    Ok(affected)
}

/// Executes a compare-and-swap [`BuiltQuery`] within a [`DatabaseTransaction`], returning
/// [`PersistenceError::VersionConflict`] if zero rows were affected.
pub async fn execute_cas_tx<T: DatabaseTransaction>(
    tx: &T,
    built_query: &BuiltQuery,
    entity: impl Into<String>,
    expected_version: i64,
) -> Result<u64, PersistenceError> {
    let affected = built_query.execute_tx(tx).await?;
    if affected == 0 {
        return Err(PersistenceError::VersionConflict {
            entity: entity.into(),
            expected: expected_version,
            actual: None,
        });
    }
    Ok(affected)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use sea_query::{Alias, Expr, ExprTrait, Order, Query};

    use super::*;
    use crate::persistence::{
        DatabaseConfig, DatabasePool, DbRow, DynamicSelect, DynamicUpdate, FromRow,
        PersistenceError, SqlDialect, SqliteBackend,
    };

    #[derive(Debug, PartialEq, Eq)]
    struct IdRow(i64);

    impl FromRow for IdRow {
        fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
            row.get::<i64>("id").map(Self)
        }
    }

    #[tokio::test]
    async fn soft_delete_filters_and_updates_in_sqlite() {
        let backend = SqliteBackend::in_memory().await.expect("sqlite in-memory");
        let pool = DatabasePool::new(backend, DatabaseConfig::new("sqlite::memory:"));

        pool.execute(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT NOT NULL, deleted_at TEXT)",
        )
        .await
        .unwrap();
        pool.execute("INSERT INTO docs (id, title, deleted_at) VALUES (1, 'active', NULL), (2, 'trashed', '2026-01-01T00:00:00Z')")
            .await
            .unwrap();

        let base_select = || {
            let mut stmt = Query::select();
            stmt.column(Alias::new("id"))
                .from(Alias::new("docs"))
                .order_by(Alias::new("id"), Order::Asc);
            DynamicSelect::new(stmt)
        };

        let active_ids: Vec<IdRow> =
            where_not_deleted(base_select(), Alias::new(DELETED_AT_COLUMN))
                .build(SqlDialect::Sqlite)
                .fetch_all_as(&pool)
                .await
                .unwrap();
        assert_eq!(active_ids, vec![IdRow(1)]);

        let deleted_ids: Vec<IdRow> =
            where_only_deleted(base_select(), Alias::new(DELETED_AT_COLUMN))
                .build(SqlDialect::Sqlite)
                .fetch_all_as(&pool)
                .await
                .unwrap();
        assert_eq!(deleted_ids, vec![IdRow(2)]);

        // Soft-delete row 1 via soft_delete_update + additional WHERE id = 1.
        let mut update = soft_delete_update(Alias::new("docs"), "2026-11-26T12:00:00Z");
        update
            .as_inner_mut()
            .and_where(Expr::col(Alias::new("id")).eq(1_i64));
        let affected = update
            .build(SqlDialect::Sqlite)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(affected, 1);

        // Soft-deleting an already deleted row must affect 0 rows (WHERE deleted_at IS NULL).
        let mut second_update = soft_delete_update(Alias::new("docs"), "2026-11-26T13:00:00Z");
        second_update
            .as_inner_mut()
            .and_where(Expr::col(Alias::new("id")).eq(1_i64));
        let second_affected = second_update
            .build(SqlDialect::Sqlite)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(second_affected, 0);

        let deleted_after: Vec<IdRow> =
            where_only_deleted(base_select(), Alias::new(DELETED_AT_COLUMN))
                .build(SqlDialect::Sqlite)
                .fetch_all_as(&pool)
                .await
                .unwrap();
        assert_eq!(deleted_after, vec![IdRow(1), IdRow(2)]);
    }

    #[tokio::test]
    async fn optimistic_lock_cas_succeeds_and_returns_version_conflict_on_stale_version() {
        let backend = SqliteBackend::in_memory().await.expect("sqlite in-memory");
        let pool = DatabasePool::new(backend, DatabaseConfig::new("sqlite::memory:"));

        pool.execute(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER NOT NULL, version INTEGER NOT NULL)",
        )
        .await
        .unwrap();
        pool.execute("INSERT INTO accounts (id, balance, version) VALUES (1, 100, 1)")
            .await
            .unwrap();

        let mut stmt = Query::update();
        stmt.table(Alias::new("accounts"))
            .value(Alias::new("balance"), 150_i64)
            .and_where(Expr::col(Alias::new("id")).eq(1_i64));
        let update = with_version_check(DynamicUpdate::new(stmt), Alias::new(VERSION_COLUMN), 1);
        let built = update.build(SqlDialect::Sqlite);
        let affected = execute_cas(&pool, &built, "accounts", 1).await.unwrap();
        assert_eq!(affected, 1);

        // Version is now 2 in DB.
        let row = pool
            .fetch_one("SELECT version FROM accounts WHERE id = 1", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<i64>("version").unwrap(), 2);

        // Retrying with stale expected version 1 must fail with PersistenceError::VersionConflict.
        let err = execute_cas(&pool, &built, "accounts", 1).await.unwrap_err();
        assert!(matches!(
            err,
            PersistenceError::VersionConflict {
                ref entity,
                expected: 1,
                actual: None,
            } if entity == "accounts"
        ));

        let app_err: rume_foundation::error::AppError = err.into();
        assert_eq!(app_err.status_code().as_u16(), 409);
    }
}
