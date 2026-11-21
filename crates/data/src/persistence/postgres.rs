//! Production PostgreSQL database backend integration using SQLx.

use std::fmt::Debug;
use std::sync::Arc;

use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{AssertSqlSafe, PgPool, Row};

use crate::persistence::backend::{
    BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue, map_sqlx_error,
};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

pub const OUTBOX_TABLE_SCHEMA: &str = include_str!("../../sql/postgres/outbox_schema.sql");
const MIGRATIONS_SCHEMA: &str = include_str!("../../sql/postgres/migrations_schema.sql");

// Per-operation PostgreSQL SQL, externalized from inline string literals.
const PG_PING_SQL: &str = include_str!("../../sql/common/ping.sql");
const PG_AUDIT_INSERT_SQL: &str = include_str!("../../sql/postgres/audit_insert.sql");

/// Untyped SQL `NULL` bind parameter for PostgreSQL.
///
/// Why: `None::<String>` sends PostgreSQL OID 25 (`TEXT`), causing `Parse` to fail
/// with a type mismatch when bound to non-text columns (e.g. `BIGINT`) on a fresh
/// connection whose statement cache has not yet inferred the parameter OID from an
/// earlier non-null bind. OID 0 (`UNSPECIFIED`) instructs PostgreSQL to infer the
/// column type from context.
struct PgNull;

impl sqlx::Type<sqlx::Postgres> for PgNull {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_oid(sqlx::postgres::types::Oid(0))
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for PgNull {
    fn encode_by_ref(
        &self,
        _buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        Ok(sqlx::encode::IsNull::Yes)
    }
}

fn bind_pg_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    params: &'q [DbValue],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    for param in params {
        query = match param {
            DbValue::Null => query.bind(PgNull),
            DbValue::Text(s) => query.bind(s.as_str()),
            DbValue::Int(i) => query.bind(*i),
            DbValue::Float(f) => query.bind(*f),
            DbValue::Bool(b) => query.bind(*b),
            DbValue::Bytes(b) => query.bind(b.as_slice()),
        };
    }
    query
}

/// Production PostgreSQL database backend backed by an asynchronous SQLx connection pool.
#[derive(Clone)]
pub struct PostgresBackend {
    pool: PgPool,
}

impl Debug for PostgresBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresBackend")
            .field("size", &self.pool.size())
            .finish()
    }
}

impl PostgresBackend {
    /// Schema DDL for the transactional outbox table in PostgreSQL.
    pub const OUTBOX_TABLE_SCHEMA: &'static str = OUTBOX_TABLE_SCHEMA;

    /// Creates a new PostgreSQL backend from connection configuration.
    pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let mut opts = PgPoolOptions::new()
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

    /// Asynchronously connects to PostgreSQL and verifies the pool.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let mut opts = PgPoolOptions::new()
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

    /// Creates a backend directly from an existing `PgPool`.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns a reference to the underlying `PgPool`.
    pub fn pool(&self) -> &PgPool {
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

fn extract_postgres_scalar(row: &PgRow) -> Option<String> {
    if let Ok(val) = row.try_get::<String, _>(0) {
        return Some(val);
    }
    if let Ok(val) = row.try_get::<i64, _>(0) {
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

impl DatabaseBackend for PostgresBackend {
    type Transaction = PostgresTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                sqlx::query(PG_PING_SQL)
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
                let query = bind_pg_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
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
            let query_obj = bind_pg_params(sqlx::query(AssertSqlSafe(query)), &params_vec);
            let row: Option<PgRow> = query_obj
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;

            Ok(row.and_then(|r| extract_postgres_scalar(&r)))
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
            let query = bind_pg_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let rows: Vec<PgRow> = query.fetch_all(&self.pool).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_pg_row_to_db_row).collect()
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
            let query = bind_pg_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
            let row: Option<PgRow> = query
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
            row.as_ref().map(sqlx_pg_row_to_db_row).transpose()
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
            sqlx::query(PG_AUDIT_INSERT_SQL)
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
            Ok(PostgresTransaction {
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
        "postgres"
    }
}

/// Active transaction handle for PostgreSQL.
#[derive(Clone)]
pub struct PostgresTransaction {
    tx: Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Postgres>>>>,
}

impl PostgresTransaction {
    /// Returns a reference to the shared mutex containing the native SQLx transaction.
    ///
    /// This escape hatch allows applications to execute driver-native SQLx queries directly
    /// while participating in the same transaction boundary.
    pub fn raw_tx(
        &self,
    ) -> &Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Postgres>>>> {
        &self.tx
    }
}

impl Debug for PostgresTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresTransaction").finish()
    }
}

impl DatabaseTransaction for PostgresTransaction {
    fn dialect(&self) -> &'static str {
        "postgres"
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
            // Why: Match `PostgresBackend::execute_params` by routing empty-param
            // calls through `sqlx::raw_sql` so unprepared/multi-statement SQL works,
            // and map all sqlx errors via `map_sqlx_error` so driver classifications
            // (`Timeout`, `Closed`, `ConnectionFailed`, `QueryFailed`) are preserved.
            // Reject transaction-control statements first so a raw batch cannot commit
            // or roll back the caller's managed transaction.
            crate::persistence::backend::validate_managed_transaction_sql(
                &statement,
                &sqlparser::dialect::PostgreSqlDialect {},
            )?;
            let res = if params_vec.is_empty() {
                sqlx::raw_sql(AssertSqlSafe(statement))
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sqlx_error)?
            } else {
                let query = bind_pg_params(sqlx::query(AssertSqlSafe(statement)), &params_vec);
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
            crate::persistence::backend::validate_managed_transaction_sql(
                &query,
                &sqlparser::dialect::PostgreSqlDialect {},
            )?;
            let query_obj = bind_pg_params(sqlx::query(AssertSqlSafe(query)), &params_vec);
            let row: Option<PgRow> = query_obj
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx_error)?;
            Ok(row.and_then(|r| extract_postgres_scalar(&r)))
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        self.query_scalar_params(query, &[])
    }

    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        let statement = statement.to_string();
        let p = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            crate::persistence::backend::validate_managed_transaction_sql(
                &statement,
                &sqlparser::dialect::PostgreSqlDialect {},
            )?;
            let query = bind_pg_params(sqlx::query(AssertSqlSafe(statement)), &p);
            let rows: Vec<PgRow> = query.fetch_all(&mut **tx).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_pg_row_to_db_row).collect()
        })
    }

    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        let statement = statement.to_string();
        let p = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            crate::persistence::backend::validate_managed_transaction_sql(
                &statement,
                &sqlparser::dialect::PostgreSqlDialect {},
            )?;
            let query = bind_pg_params(sqlx::query(AssertSqlSafe(statement)), &p);
            let row: Option<PgRow> = query
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx_error)?;
            row.as_ref().map(sqlx_pg_row_to_db_row).transpose()
        })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.take().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            tx.commit()
                .await
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            Ok(())
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.take().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            tx.rollback()
                .await
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            Ok(())
        })
    }
}

pub(crate) fn sqlx_pg_row_to_db_row(row: &PgRow) -> Result<DbRow, PersistenceError> {
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
                "BOOL" => row.try_get::<bool, _>(i).map(DbValue::Bool).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode BOOL: {e}"),
                    }
                })?,
                "INT2" => row
                    .try_get::<i16, _>(i)
                    .map(|n| DbValue::Int(n as i64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode INT2: {e}"),
                    })?,
                "INT4" => row
                    .try_get::<i32, _>(i)
                    .map(|n| DbValue::Int(n as i64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode INT4: {e}"),
                    })?,
                "INT8" => row.try_get::<i64, _>(i).map(DbValue::Int).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode INT8: {e}"),
                    }
                })?,
                "FLOAT4" => row
                    .try_get::<f32, _>(i)
                    .map(|f| DbValue::Float(f as f64))
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode FLOAT4: {e}"),
                    })?,
                "FLOAT8" => row.try_get::<f64, _>(i).map(DbValue::Float).map_err(|e| {
                    PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode FLOAT8: {e}"),
                    }
                })?,
                "BYTEA" => row
                    .try_get::<Vec<u8>, _>(i)
                    .map(DbValue::Bytes)
                    .map_err(|e| PersistenceError::DecodeError {
                        column: col_name.clone(),
                        message: format!("failed to decode BYTEA: {e}"),
                    })?,
                "VARCHAR" | "CHAR" | "TEXT" | "NAME" => row
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
