//! Production MySQL database backend integration using SQLx.

use std::fmt::Debug;
use std::sync::Arc;

use sqlx::mysql::{MySqlPoolOptions, MySqlRow};
use sqlx::{AssertSqlSafe, MySqlPool, Row};

use crate::persistence::backend::{
    BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue, map_sqlx_error,
};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

pub const OUTBOX_TABLE_SCHEMA: &str = include_str!("sql/mysql/outbox_schema.sql");
const MIGRATIONS_SCHEMA: &str = include_str!("sql/mysql/migrations_schema.sql");

// Per-operation MySQL SQL, externalized from inline string literals.
// Why: All MySQL queries (including lock cleanup and lease-expiry lookup) live in
// `sql/mysql/*.sql` so this module contains no divergent inline SQL literals.
const MYSQL_PING_SQL: &str = include_str!("sql/common/ping.sql");
const MYSQL_MIGRATION_UNLOCK_SQL: &str = include_str!("sql/mysql/mysql_migration_unlock.sql");
const MYSQL_AUDIT_INSERT_SQL: &str = include_str!("sql/mysql/audit_insert.sql");

/// Production MySQL and MariaDB database backend backed by an asynchronous SQLx connection pool.
#[derive(Clone)]
pub struct MysqlBackend {
    pool: MySqlPool,
}

/// Type alias for [`MysqlBackend`], which natively supports MariaDB 10.6+ via SQLx.
pub type MariadbBackend = MysqlBackend;

/// Type alias for [`MysqlTransaction`].
pub type MariadbTransaction = MysqlTransaction;

impl Debug for MysqlBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlBackend")
            .field("size", &self.pool.size())
            .finish()
    }
}

impl MysqlBackend {
    /// Schema DDL for the transactional outbox table in MySQL.
    pub const OUTBOX_TABLE_SCHEMA: &'static str = OUTBOX_TABLE_SCHEMA;

    /// Creates a new MySQL backend from connection configuration.
    pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let mut opts = MySqlPoolOptions::new()
            .max_connections(config.max_connections.max(1))
            .min_connections(config.min_connections)
            .acquire_timeout(config.connect_timeout());
        if let Some(idle) = config.idle_timeout() {
            opts = opts.idle_timeout(idle);
        }

        let pool = opts
            .connect_lazy(&config.url)
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

        Ok(Self { pool })
    }

    /// Asynchronously connects to MySQL and verifies the pool.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let mut opts = MySqlPoolOptions::new()
            .max_connections(config.max_connections.max(1))
            .min_connections(config.min_connections)
            .acquire_timeout(config.connect_timeout());
        if let Some(idle) = config.idle_timeout() {
            opts = opts.idle_timeout(idle);
        }

        let pool = opts
            .connect(&config.url)
            .await
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

        Ok(Self { pool })
    }

    /// Creates a backend directly from an existing `MySqlPool`.
    pub fn from_pool(pool: MySqlPool) -> Self {
        Self { pool }
    }

    /// Returns a reference to the underlying `MySqlPool`.
    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// Initializes the `rume_outbox` table if it does not already exist.
    pub async fn init_outbox_table(&self) -> Result<(), PersistenceError> {
        self.execute(OUTBOX_TABLE_SCHEMA).await?;
        Ok(())
    }

    /// Initializes the `rume_migrations` table if it does not already exist.
    pub async fn init_migrations_table(&self) -> Result<(), PersistenceError> {
        self.execute(MIGRATIONS_SCHEMA).await?;
        Ok(())
    }
}

fn bind_mysql_params<'q>(
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

fn extract_mysql_scalar(row: &MySqlRow) -> Option<String> {
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

impl DatabaseBackend for MysqlBackend {
    type Transaction = MysqlTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                sqlx::query(MYSQL_PING_SQL)
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
                let query = bind_mysql_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
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
            let query_obj = bind_mysql_params(sqlx::query(AssertSqlSafe(query)), &params_vec);
            let row: Option<MySqlRow> = query_obj
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;

            Ok(row.and_then(|r| extract_mysql_scalar(&r)))
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
            let query = bind_mysql_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let rows: Vec<MySqlRow> = query.fetch_all(&self.pool).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_mysql_row_to_db_row).collect()
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
            let query = bind_mysql_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let row: Option<MySqlRow> = query
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
            row.as_ref().map(sqlx_mysql_row_to_db_row).transpose()
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
            sqlx::query(MYSQL_AUDIT_INSERT_SQL)
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
            Ok(MysqlTransaction {
                tx: Arc::new(tokio::sync::Mutex::new(Some(tx))),
                release_lock_on_drop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        "mysql"
    }
}

/// Active transaction handle for MySQL.
#[derive(Clone)]
pub struct MysqlTransaction {
    tx: Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::MySql>>>>,
    release_lock_on_drop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for MysqlTransaction {
    fn drop(&mut self) {
        if self
            .release_lock_on_drop
            .load(std::sync::atomic::Ordering::SeqCst)
            && Arc::strong_count(&self.tx) == 1
            && let Ok(mut guard) = self.tx.try_lock()
            && let Some(mut tx) = guard.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            // Cancellation-safe cleanup: if a migration future is dropped while holding
            // `rume_migrations_lock`, release the session-bound MySQL named lock before
            // returning the connection to the pool.
            handle.spawn(async move {
                let _ = sqlx::query(MYSQL_MIGRATION_UNLOCK_SQL)
                    .execute(&mut *tx)
                    .await;
                let _ = tx.rollback().await;
            });
        }
    }
}

impl MysqlTransaction {
    /// Returns a reference to the shared mutex containing the native SQLx transaction.
    ///
    /// This escape hatch allows applications to execute driver-native SQLx queries directly
    /// while participating in the same transaction boundary.
    pub fn raw_tx(
        &self,
    ) -> &Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::MySql>>>> {
        &self.tx
    }
}

impl Debug for MysqlTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlTransaction").finish()
    }
}

impl DatabaseTransaction for MysqlTransaction {
    fn dialect(&self) -> &'static str {
        "mysql"
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
            // Why: Match `MysqlBackend::execute_params` by routing empty-param
            // calls through `sqlx::raw_sql` for unprepared SQL and mapping all
            // sqlx errors via `map_sqlx_error` to preserve error classification.
            // Validate managed transaction SQL first (matching `PostgresTransaction`)
            // so raw batches cannot commit or roll back the caller's managed transaction.
            let res = if params_vec.is_empty() {
                crate::persistence::backend::validate_managed_transaction_sql(
                    &statement,
                    &sqlparser::dialect::MySqlDialect {},
                )?;
                sqlx::raw_sql(AssertSqlSafe(statement))
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sqlx_error)?
            } else {
                let query = bind_mysql_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
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
            let query_obj = bind_mysql_params(sqlx::query(AssertSqlSafe(query)), &params_vec);
            let row: Option<MySqlRow> = query_obj
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx_error)?;
            Ok(row.and_then(|r| extract_mysql_scalar(&r)))
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
            let query = bind_mysql_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let rows: Vec<MySqlRow> = query.fetch_all(&mut **tx).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_mysql_row_to_db_row).collect()
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
            let query = bind_mysql_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let row: Option<MySqlRow> = query
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx_error)?;
            row.as_ref().map(sqlx_mysql_row_to_db_row).transpose()
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let tx_arc = Arc::clone(&self.tx);
        let release_flag = Arc::clone(&self.release_lock_on_drop);
        Box::pin(async move {
            release_flag.store(false, std::sync::atomic::Ordering::SeqCst);
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
        let release_flag = Arc::clone(&self.release_lock_on_drop);
        Box::pin(async move {
            release_flag.store(false, std::sync::atomic::Ordering::SeqCst);
            let mut guard = tx_arc.lock().await;
            if let Some(tx) = guard.take() {
                tx.rollback()
                    .await
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            }
            Ok(())
        })
    }

    fn release_mysql_lock_on_drop(&self, armed: bool) {
        self.release_lock_on_drop
            .store(armed, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn sqlx_mysql_row_to_db_row(row: &MySqlRow) -> Result<DbRow, PersistenceError> {
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
                "TINYINT" => row
                    .try_get::<i8, _>(i)
                    .map(|n| DbValue::Int(n as i64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode TINYINT: {e}"),
                    })?,
                "SMALLINT" => row
                    .try_get::<i16, _>(i)
                    .map(|n| DbValue::Int(n as i64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode SMALLINT: {e}"),
                    })?,
                "INT" => row
                    .try_get::<i32, _>(i)
                    .map(|n| DbValue::Int(n as i64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode INT: {e}"),
                    })?,
                "BIGINT" => {
                    if let Ok(n) = row.try_get::<i64, _>(i) {
                        DbValue::Int(n)
                    } else if let Ok(u) = row.try_get::<u64, _>(i) {
                        if let Ok(signed) = i64::try_from(u) {
                            DbValue::Int(signed)
                        } else {
                            DbValue::Text(u.to_string())
                        }
                    } else {
                        return Err(PersistenceError::DecodeError {
                            column: col_name.clone(),
                            message: "failed to decode BIGINT".into(),
                        });
                    }
                }
                "FLOAT" => row
                    .try_get::<f32, _>(i)
                    .map(|f| DbValue::Float(f as f64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode FLOAT: {e}"),
                    })?,
                "DOUBLE" => row.try_get::<f64, _>(i).map(DbValue::Float).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode DOUBLE: {e}"),
                    }
                })?,
                "BLOB" | "VARBINARY" | "BINARY" => row
                    .try_get::<Vec<u8>, _>(i)
                    .map(DbValue::Bytes)
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode binary column: {e}"),
                    })?,
                "VARCHAR" | "CHAR" | "TEXT" => row
                    .try_get::<String, _>(i)
                    .map(DbValue::Text)
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode text column: {e}"),
                    })?,
                _ => {
                    if let Ok(s) = row.try_get::<String, _>(i) {
                        DbValue::Text(s)
                    } else if let Ok(n) = row.try_get::<i64, _>(i) {
                        DbValue::Int(n)
                    } else if let Ok(u) = row.try_get::<u64, _>(i) {
                        if let Ok(signed) = i64::try_from(u) {
                            DbValue::Int(signed)
                        } else {
                            DbValue::Text(u.to_string())
                        }
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
