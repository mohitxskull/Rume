//! Embedded SQLite database backend integration using SQLx.

use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{AssertSqlSafe, Row, SqlitePool};

use crate::persistence::backend::{
    BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue, map_sqlx_error,
};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

const OUTBOX_SCHEMA: &str = include_str!("sql/sqlite/outbox_schema.sql");
const MIGRATIONS_SCHEMA: &str = include_str!("sql/sqlite/migrations_schema.sql");

// Per-operation SQLite SQL, externalized from inline string literals.
const SQLITE_PING_SQL: &str = include_str!("sql/common/ping.sql");
const SQLITE_AUDIT_INSERT_SQL: &str = include_str!("sql/sqlite/audit_insert.sql");

/// Embedded SQLite database backend backed by an asynchronous SQLx connection pool.
#[derive(Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl Debug for SqliteBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteBackend")
            .field("size", &self.pool.size())
            .finish()
    }
}

impl SqliteBackend {
    /// Creates a new in-memory SQLite backend.
    pub async fn in_memory() -> Result<Self, PersistenceError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_millis(5000));

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

        Ok(Self { pool })
    }

    /// Opens an SQLite database at the given file path.
    pub async fn open(path: &str) -> Result<Self, PersistenceError> {
        let config = DatabaseConfig::new(path);
        Self::new(&config).await
    }

    /// Creates an SQLite backend from a [`DatabaseConfig`].
    pub async fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let url = config.url.trim();
        if url == "sqlite::memory:" || url == ":memory:" || url == "memory://" || url.is_empty() {
            Self::in_memory().await
        } else {
            let path_url = if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
                url.to_string()
            } else {
                format!("sqlite://{url}")
            };
            let opts = SqliteConnectOptions::from_str(&path_url)
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .foreign_keys(true)
                .busy_timeout(Duration::from_millis(5000));

            let mut pool_opts = SqlitePoolOptions::new()
                .max_connections(config.max_connections.max(1))
                .min_connections(config.min_connections)
                .acquire_timeout(config.connect_timeout());
            if let Some(idle) = config.idle_timeout() {
                pool_opts = pool_opts.idle_timeout(idle);
            }

            let pool = pool_opts
                .connect_with(opts)
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

            Ok(Self { pool })
        }
    }

    /// Creates a backend directly from an existing `SqlitePool`.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Returns a reference to the underlying `SqlitePool`.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Initializes the `rume_outbox` table if it does not already exist.
    pub async fn init_outbox_table(&self) -> Result<(), PersistenceError> {
        self.execute(OUTBOX_SCHEMA).await?;
        Ok(())
    }

    /// Initializes the `rume_migrations` table if it does not already exist.
    pub async fn init_migrations_table(&self) -> Result<(), PersistenceError> {
        self.execute(MIGRATIONS_SCHEMA).await?;
        Ok(())
    }
}

fn bind_sqlite_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    params: &'q [DbValue],
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
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

fn extract_sqlite_scalar(row: &SqliteRow) -> Option<String> {
    if let Ok(val) = row.try_get::<String, _>(0) {
        return Some(val);
    }
    if let Ok(val) = row.try_get::<i64, _>(0) {
        return Some(val.to_string());
    }
    if let Ok(val) = row.try_get::<i32, _>(0) {
        return Some(val.to_string());
    }
    if let Ok(val) = row.try_get::<f64, _>(0) {
        return Some(val.to_string());
    }
    if let Ok(val) = row.try_get::<bool, _>(0) {
        return Some(val.to_string());
    }
    None
}

impl DatabaseBackend for SqliteBackend {
    type Transaction = SqliteTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                sqlx::query(SQLITE_PING_SQL)
                    .execute(&self.pool)
                    .await
                    .map_err(map_sqlx_error)
            })
            .await
            .map_err(|_| PersistenceError::Timeout)?
            .map(|_| ())
        })
    }

    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let statement = statement.to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            if params_vec.is_empty() {
                let res = sqlx::raw_sql(AssertSqlSafe(statement.clone()))
                    .execute(&self.pool)
                    .await
                    .map_err(map_sqlx_error)?;
                Ok(res.rows_affected())
            } else {
                let query = bind_sqlite_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
                let res = query.execute(&self.pool).await.map_err(map_sqlx_error)?;
                Ok(res.rows_affected())
            }
        })
    }

    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let query = query.trim().to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            let query_obj = bind_sqlite_params(sqlx::query(AssertSqlSafe(query)), &params_vec);
            let row: Option<SqliteRow> = query_obj
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;

            Ok(row.and_then(|r| extract_sqlite_scalar(&r)))
        })
    }

    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        let statement = statement.to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            let query = bind_sqlite_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let rows: Vec<SqliteRow> = query.fetch_all(&self.pool).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_sqlite_row_to_db_row).collect()
        })
    }

    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        let statement = statement.to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            let query = bind_sqlite_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let row: Option<SqliteRow> = query
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
            row.as_ref().map(sqlx_sqlite_row_to_db_row).transpose()
        })
    }

    fn record_audit_log(
        &self,
        id: &str,
        action: &str,
        resource: &str,
        status: &str,
        actor_id: Option<&str>,
        target_id: Option<&str>,
        ip_address: Option<&str>,
        metadata: &str,
        created_at: i64,
    ) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let id = id.to_string();
        let action = action.to_string();
        let resource = resource.to_string();
        let status = status.to_string();
        let actor_id = actor_id.map(|s| s.to_string());
        let target_id = target_id.map(|s| s.to_string());
        let ip_address = ip_address.map(|s| s.to_string());
        let metadata = metadata.to_string();
        Box::pin(async move {
            sqlx::query(SQLITE_AUDIT_INSERT_SQL)
                .bind(id)
                .bind(action)
                .bind(resource)
                .bind(status)
                .bind(actor_id)
                .bind(target_id)
                .bind(ip_address)
                .bind(metadata)
                .bind(created_at)
                .execute(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
            Ok(())
        })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        Box::pin(async move {
            let tx = self.pool.begin().await.map_err(map_sqlx_error)?;
            Ok(SqliteTransaction {
                tx: Arc::new(tokio::sync::Mutex::new(Some(tx))),
            })
        })
    }

    fn pool_size(&self) -> u32 {
        self.pool.size()
    }

    fn pool_idle(&self) -> u32 {
        self.pool.num_idle() as u32
    }

    fn is_closed(&self) -> bool {
        self.pool.is_closed()
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.pool.close().await;
        })
    }

    fn dialect(&self) -> &'static str {
        "sqlite"
    }
}

/// Active transaction handle for SQLite.
#[derive(Clone)]
pub struct SqliteTransaction {
    tx: Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Sqlite>>>>,
}

impl SqliteTransaction {
    /// Returns a reference to the shared mutex containing the native SQLx transaction.
    ///
    /// This escape hatch allows applications to execute driver-native SQLx queries directly
    /// while participating in the same transaction boundary.
    pub fn raw_tx(
        &self,
    ) -> &Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Sqlite>>>> {
        &self.tx
    }
}

impl Debug for SqliteTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTransaction").finish()
    }
}

impl DatabaseTransaction for SqliteTransaction {
    fn dialect(&self) -> &'static str {
        "sqlite"
    }

    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let statement = statement.to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            // Why: Match `SqliteBackend::execute_params` by routing empty-param
            // calls through `sqlx::raw_sql` for unprepared/multi-statement SQL
            // and mapping all sqlx errors via `map_sqlx_error` to preserve
            // driver error classification. Enforce `validate_managed_transaction_sql`
            // (matching `PostgresTransaction` and `MysqlTransaction`) so raw SQL
            // batches cannot resolve the managed transaction out-of-band, and reject
            // unbound SQLite placeholders first so `raw_sql` cannot silently bind them
            // as NULL.
            let res = if params_vec.is_empty() {
                crate::persistence::backend::validate_managed_transaction_sql(
                    &statement,
                    &sqlparser::dialect::SQLiteDialect {},
                )?;
                reject_unbound_sqlite_placeholders(&statement)?;
                sqlx::raw_sql(AssertSqlSafe(statement))
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sqlx_error)?
            } else {
                let query = bind_sqlite_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
                query.execute(&mut **tx).await.map_err(map_sqlx_error)?
            };
            Ok(res.rows_affected())
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        self.execute_params(statement, &[])
    }

    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let query = query.trim().to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let query_obj = bind_sqlite_params(sqlx::query(AssertSqlSafe(query)), &params_vec);
            let row: Option<SqliteRow> = query_obj
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx_error)?;
            Ok(row.and_then(|r| extract_sqlite_scalar(&r)))
        })
    }

    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        let statement = statement.to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let query = bind_sqlite_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let rows: Vec<SqliteRow> = query.fetch_all(&mut **tx).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_sqlite_row_to_db_row).collect()
        })
    }

    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        let statement = statement.to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let query = bind_sqlite_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let row: Option<SqliteRow> = query
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx_error)?;
            row.as_ref().map(sqlx_sqlite_row_to_db_row).transpose()
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            if let Some(tx) = guard.take() {
                tx.commit()
                    .await
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            }
            Ok(())
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            if let Some(tx) = guard.take() {
                tx.rollback()
                    .await
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            }
            Ok(())
        })
    }
}

pub(crate) fn sqlx_sqlite_row_to_db_row(row: &SqliteRow) -> Result<DbRow, PersistenceError> {
    use sqlx::{Column, Row, TypeInfo, ValueRef};
    let columns: Vec<String> = row.columns().iter().map(|c| c.name().to_string()).collect();
    let mut values = Vec::with_capacity(columns.len());
    for (i, col_name) in columns.iter().enumerate() {
        let raw = row
            .try_get_raw(i)
            .map_err(|e| PersistenceError::DecodeError {
                column: col_name.clone(),
                message: format!("failed to read raw column: {e}"),
            })?;
        let val = if raw.is_null() {
            DbValue::Null
        } else {
            let type_info = raw.type_info();
            let type_name = type_info.name();
            match type_name {
                "BOOLEAN" => row.try_get::<bool, _>(i).map(DbValue::Bool).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode BOOLEAN: {e}"),
                    }
                })?,
                "INTEGER" => row.try_get::<i64, _>(i).map(DbValue::Int).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode INTEGER: {e}"),
                    }
                })?,
                "REAL" => row.try_get::<f64, _>(i).map(DbValue::Float).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode REAL: {e}"),
                    }
                })?,
                "BLOB" => row
                    .try_get::<Vec<u8>, _>(i)
                    .map(DbValue::Bytes)
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode BLOB: {e}"),
                    })?,
                "TEXT" => row
                    .try_get::<String, _>(i)
                    .map(DbValue::Text)
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode TEXT: {e}"),
                    })?,
                _ => {
                    if let Ok(s) = row.try_get::<String, _>(i) {
                        DbValue::Text(s)
                    } else if let Ok(n) = row.try_get::<i64, _>(i) {
                        DbValue::Int(n)
                    } else if let Ok(f) = row.try_get::<f64, _>(i) {
                        DbValue::Float(f)
                    } else if let Ok(b) = row.try_get::<bool, _>(i) {
                        DbValue::Bool(b)
                    } else if let Ok(bytes) = row.try_get::<Vec<u8>, _>(i) {
                        DbValue::Bytes(bytes)
                    } else {
                        return Err(PersistenceError::DecodeError {
                            column: col_name.clone(),
                            message: format!(
                                "unsupported or undecodable column type '{type_name}'"
                            ),
                        });
                    }
                }
            }
        };
        values.push(val);
    }
    Ok(DbRow::new(columns, values))
}

/// Rejects SQLite bind placeholders (`?`, `?NNN`, `:name`, `@name`, `$name`) outside
/// string literals, quoted identifiers, and comments when executing with empty params.
///
/// Why: Delegate SQLite lexical scanning (comments, single/double/backtick quotes, `[...]`
/// identifiers) to `sqlparser = "=0.53.0"` instead of maintaining a hand-rolled byte scanner.
/// `SQLiteDialect` tokenizes `?`, `?NNN`, and `$name` as `Token::Placeholder`, while `:name`
/// and `@name` tokenize as `Token::Colon` / `Token::AtSign` immediately adjacent (`tok.span.end
/// == next.span.start`) to an unquoted `Token::Word` (plus `Token::Number` after `@`). Fails
/// closed with `PersistenceError::QueryFailed` on `TokenizerError` (revisit only if `sqlparser`
/// natively tokenizes `:name` / `@name` as `Token::Placeholder` under `SQLiteDialect`).
pub(crate) fn reject_unbound_sqlite_placeholders(statement: &str) -> Result<(), PersistenceError> {
    use sqlparser::dialect::SQLiteDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    let tokens = Tokenizer::new(&SQLiteDialect {}, statement)
        .tokenize_with_location()
        .map_err(|e| {
            PersistenceError::QueryFailed(format!("failed to tokenize SQLite SQL: {e}"))
        })?;

    for (idx, tok) in tokens.iter().enumerate() {
        match &tok.token {
            Token::Placeholder(placeholder) => {
                return Err(PersistenceError::QueryFailed(format!(
                    "statement contains unbound SQLite placeholder '{placeholder}' with empty parameters"
                )));
            }
            Token::Colon => {
                if let Some(next) = tokens.get(idx + 1)
                    && tok.span.end == next.span.start
                    && matches!(&next.token, Token::Word(w) if w.quote_style.is_none())
                {
                    return Err(PersistenceError::QueryFailed(
                        "statement contains unbound SQLite named placeholder ':' with empty parameters"
                            .into(),
                    ));
                }
            }
            Token::AtSign => {
                if let Some(next) = tokens.get(idx + 1)
                    && tok.span.end == next.span.start
                    && (matches!(&next.token, Token::Word(w) if w.quote_style.is_none())
                        || matches!(&next.token, Token::Number(_, _)))
                {
                    return Err(PersistenceError::QueryFailed(
                        "statement contains unbound SQLite named placeholder '@' with empty parameters"
                            .into(),
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_tx_rejects_unbound_placeholders_with_empty_params() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let backend = SqliteBackend::from_pool(pool);

        backend
            .execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, note TEXT)")
            .await
            .expect("create table");
        backend
            .execute("INSERT INTO notes (id, note) VALUES (1, 'original')")
            .await
            .expect("seed row");

        let tx = backend.begin_transaction().await.expect("begin tx");

        for stmt in [
            "UPDATE notes SET note = :note WHERE id = 1",
            "UPDATE notes SET note = ? WHERE id = 1",
            "UPDATE notes SET note = ?1 WHERE id = 1",
            "UPDATE notes SET note = @note WHERE id = 1",
            "UPDATE notes SET note = @1 WHERE id = 1",
            "UPDATE notes SET note = $note WHERE id = 1",
        ] {
            let err = tx
                .execute_params(stmt, &[])
                .await
                .expect_err(&format!("expected unbound placeholder error for: {stmt}"));
            assert!(
                matches!(err, PersistenceError::QueryFailed(_)),
                "expected QueryFailed for {stmt}, got {err:?}"
            );
        }

        // Quoted literals, quoted identifiers, and comments containing placeholder-like tokens must pass.
        tx.execute_params(
            "/* :note ? */ -- @note $note\nUPDATE notes SET note = 'kept :note ? @a $b' WHERE id = 1",
            &[],
        )
        .await
        .expect("quoted/commented placeholders should be allowed");

        tx.execute_params(
            "CREATE TABLE quoted_ph ([col?] TEXT, `col:name` TEXT, \"col@name\" TEXT);",
            &[],
        )
        .await
        .expect("quoted identifiers containing placeholder characters must pass");

        // Multi-statement raw execution without placeholders continues to work.
        tx.execute("CREATE TABLE extra (id INTEGER); INSERT INTO extra (id) VALUES (42);")
            .await
            .expect("multi-statement batch without placeholders should succeed");

        tx.commit().await.expect("commit tx");

        let note = backend
            .query_scalar("SELECT note FROM notes WHERE id = 1")
            .await
            .expect("query note");
        assert_eq!(note.as_deref(), Some("kept :note ? @a $b"));
    }

    #[tokio::test]
    async fn sqlite_tx_rejects_managed_transaction_control_sql() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let backend = SqliteBackend::from_pool(pool);
        let tx = backend.begin_transaction().await.expect("begin tx");

        for stmt in ["COMMIT;", "BEGIN;", "ROLLBACK;"] {
            let err = tx
                .execute_params(stmt, &[])
                .await
                .expect_err(&format!("expected TransactionError for {stmt}"));
            assert!(
                matches!(err, PersistenceError::TransactionError(_)),
                "expected TransactionError for {stmt}, got {err:?}"
            );
        }

        // Quoted `[BEGIN]` identifier in SQLite must still execute cleanly.
        tx.execute_params("CREATE TABLE [BEGIN] (id INTEGER PRIMARY KEY);", &[])
            .await
            .expect("bracketed identifier [BEGIN] should be allowed");
        tx.rollback().await.expect("rollback tx");
    }
}

#[cfg(test)]
mod data_sql_scanners_proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn reject_unbound_sqlite_placeholders_never_panics_on_arbitrary_utf8(s in "\\PC*") {
            let _ = reject_unbound_sqlite_placeholders(&s);
        }

        #[test]
        fn reject_unbound_sqlite_placeholders_always_rejects_unbound_markers(
            ident in "[a-zA-Z_][a-zA-Z0-9_]{0,14}",
            digits in "[0-9]{1,4}",
        ) {
            for ph in [
                "?".to_string(),
                format!("?{digits}"),
                format!(":{ident}"),
                format!("@{ident}"),
                format!("@{digits}"),
                format!("${ident}"),
            ] {
                let sql = format!("SELECT * FROM t WHERE a = {ph}");
                let err = reject_unbound_sqlite_placeholders(&sql)
                    .expect_err(&format!("expected QueryFailed for '{sql}'"));
                prop_assert!(matches!(err, PersistenceError::QueryFailed(_)));
            }
        }

        #[test]
        fn reject_unbound_sqlite_placeholders_permits_markers_in_quotes_comments_and_ids(
            ident in "[a-zA-Z_][a-zA-Z0-9_]{0,10}",
            pad in "[a-zA-Z0-9_ ]{0,8}",
        ) {
            let markers = format!("? ?1 :{ident} @{ident} ${ident}");
            for sql in [
                format!("SELECT '{pad} {markers} {pad}';"),
                format!("-- {pad} {markers}\nSELECT 1;"),
                format!("/* {pad} {markers} */ SELECT 1;"),
                format!("SELECT \"{ident}?\" FROM t;"),
                format!("SELECT `{ident}:{ident}` FROM t;"),
                format!("SELECT [{ident}@{ident}] FROM t;"),
            ] {
                prop_assert!(
                    reject_unbound_sqlite_placeholders(&sql).is_ok(),
                    "quoted/commented markers must be permitted: {sql}"
                );
            }
        }
    }
}
