//! Embedded SQLite database backend integration using SQLx.

use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};
use crate::persistence::backend::{
    BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue, map_sqlx_error,
};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

const OUTBOX_SCHEMA: &str = include_str!("sql/sqlite/outbox_schema.sql");
const OUTBOX_CLAIM_SQL: &str = include_str!("sql/sqlite/outbox_claim.sql");
const MIGRATIONS_SCHEMA: &str = include_str!("sql/sqlite/migrations_schema.sql");
const INSERT_OUTBOX_SQL: &str = include_str!("sql/sqlite/outbox_insert.sql");
const OUTBOX_RECOVER_SQL: &str = include_str!("sql/sqlite/outbox_recover.sql");

// Per-operation SQLite SQL, externalized from inline string literals.
const SQLITE_PING_SQL: &str = include_str!("sql/common/ping.sql");
const SQLITE_AUDIT_INSERT_SQL: &str = include_str!("sql/sqlite/audit_insert.sql");
const SQLITE_OUTBOX_CLAIM_LEASE_SQL: &str = include_str!("sql/sqlite/outbox_claim_lease.sql");
const SQLITE_OUTBOX_RENEW_LEASE_SQL: &str = include_str!("sql/sqlite/outbox_renew_lease.sql");
const SQLITE_OUTBOX_MARK_PUBLISHED_SQL: &str = include_str!("sql/sqlite/outbox_mark_published.sql");
const SQLITE_OUTBOX_SELECT_RETRY_COUNT_SQL: &str =
    include_str!("sql/sqlite/outbox_select_retry_count.sql");
const SQLITE_OUTBOX_MARK_FAILED_SQL: &str = include_str!("sql/sqlite/outbox_mark_failed.sql");
const SQLITE_OUTBOX_PENDING_COUNT_SQL: &str = include_str!("sql/sqlite/outbox_pending_count.sql");
const SQLITE_OUTBOX_PUBLISHED_COUNT_SQL: &str =
    include_str!("sql/sqlite/outbox_published_count.sql");
const SQLITE_OUTBOX_FAILED_COUNT_SQL: &str = include_str!("sql/sqlite/outbox_failed_count.sql");
const SQLITE_OUTBOX_FETCH_FAILED_SQL: &str = include_str!("sql/sqlite/outbox_fetch_failed.sql");
const SQLITE_OUTBOX_REPROCESS_FAILED_SQL: &str =
    include_str!("sql/sqlite/outbox_reprocess_failed.sql");
const SQLITE_OUTBOX_PURGE_PUBLISHED_SQL: &str =
    include_str!("sql/sqlite/outbox_purge_published.sql");

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
        let stmt = statement.to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            if params_vec.is_empty() {
                let res = sqlx::raw_sql(&stmt)
                    .execute(&self.pool)
                    .await
                    .map_err(map_sqlx_error)?;
                Ok(res.rows_affected())
            } else {
                let mut query = sqlx::query(&stmt);
                for param in &params_vec {
                    query = match param {
                        DbValue::Null => query.bind(None as Option<String>),
                        DbValue::Text(s) => query.bind(s.as_str()),
                        DbValue::Int(i) => query.bind(*i),
                        DbValue::Float(f) => query.bind(*f),
                        DbValue::Bool(b) => query.bind(*b),
                        DbValue::Bytes(b) => query.bind(b.as_slice()),
                    };
                }
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
        let q = query.trim().to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            let mut query_obj = sqlx::query(&q);
            for param in &params_vec {
                query_obj = match param {
                    DbValue::Null => query_obj.bind(None as Option<String>),
                    DbValue::Text(s) => query_obj.bind(s.as_str()),
                    DbValue::Int(i) => query_obj.bind(*i),
                    DbValue::Float(f) => query_obj.bind(*f),
                    DbValue::Bool(b) => query_obj.bind(*b),
                    DbValue::Bytes(b) => query_obj.bind(b.as_slice()),
                };
            }
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
        let stmt = statement.to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            let mut query = sqlx::query(&stmt);
            for param in &params_vec {
                query = match param {
                    DbValue::Null => query.bind(None as Option<String>),
                    DbValue::Text(s) => query.bind(s.as_str()),
                    DbValue::Int(i) => query.bind(*i),
                    DbValue::Float(f) => query.bind(*f),
                    DbValue::Bool(b) => query.bind(*b),
                    DbValue::Bytes(b) => query.bind(b.as_slice()),
                };
            }
            let rows: Vec<SqliteRow> = query.fetch_all(&self.pool).await.map_err(map_sqlx_error)?;
            rows.iter().map(sqlx_sqlite_row_to_db_row).collect()
        })
    }

    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        let stmt = statement.to_string();
        let params_vec = params.to_vec();
        Box::pin(async move {
            let mut query = sqlx::query(&stmt);
            for param in &params_vec {
                query = match param {
                    DbValue::Null => query.bind(None as Option<String>),
                    DbValue::Text(s) => query.bind(s.as_str()),
                    DbValue::Int(i) => query.bind(*i),
                    DbValue::Float(f) => query.bind(*f),
                    DbValue::Bool(b) => query.bind(*b),
                    DbValue::Bytes(b) => query.bind(b.as_slice()),
                };
            }
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
        let stmt = statement.to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let mut query = sqlx::query(&stmt);
            for param in &params_vec {
                query = match param {
                    DbValue::Null => query.bind(None as Option<String>),
                    DbValue::Text(s) => query.bind(s.as_str()),
                    DbValue::Int(i) => query.bind(*i),
                    DbValue::Float(f) => query.bind(*f),
                    DbValue::Bool(b) => query.bind(*b),
                    DbValue::Bytes(b) => query.bind(b.as_slice()),
                };
            }
            let res = query
                .execute(&mut **tx)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(res.rows_affected())
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            use sqlx::Executor;
            let res = (&mut **tx)
                .execute(stmt.as_str())
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(res.rows_affected())
        })
    }

    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.trim().to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let mut query_obj = sqlx::query(&q);
            for param in &params_vec {
                query_obj = match param {
                    DbValue::Null => query_obj.bind(None as Option<String>),
                    DbValue::Text(s) => query_obj.bind(s.as_str()),
                    DbValue::Int(i) => query_obj.bind(*i),
                    DbValue::Float(f) => query_obj.bind(*f),
                    DbValue::Bool(b) => query_obj.bind(*b),
                    DbValue::Bytes(b) => query_obj.bind(b.as_slice()),
                };
            }
            let row: Option<SqliteRow> = query_obj
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(row.and_then(|r| extract_sqlite_scalar(&r)))
        })
    }

    fn fetch_all(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        let stmt = statement.to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let mut query = sqlx::query(&stmt);
            for param in &params_vec {
                query = match param {
                    DbValue::Null => query.bind(None as Option<String>),
                    DbValue::Text(s) => query.bind(s.as_str()),
                    DbValue::Int(i) => query.bind(*i),
                    DbValue::Float(f) => query.bind(*f),
                    DbValue::Bool(b) => query.bind(*b),
                    DbValue::Bytes(b) => query.bind(b.as_slice()),
                };
            }
            let rows: Vec<SqliteRow> = query
                .fetch_all(&mut **tx)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            rows.iter().map(sqlx_sqlite_row_to_db_row).collect()
        })
    }

    fn fetch_optional(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        let stmt = statement.to_string();
        let params_vec = params.to_vec();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard.as_mut().ok_or_else(|| {
                PersistenceError::TransactionError("transaction already resolved".into())
            })?;
            let mut query = sqlx::query(&stmt);
            for param in &params_vec {
                query = match param {
                    DbValue::Null => query.bind(None as Option<String>),
                    DbValue::Text(s) => query.bind(s.as_str()),
                    DbValue::Int(i) => query.bind(*i),
                    DbValue::Float(f) => query.bind(*f),
                    DbValue::Bool(b) => query.bind(*b),
                    DbValue::Bytes(b) => query.bind(b.as_slice()),
                };
            }
            let row: Option<SqliteRow> = query
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
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

fn parse_sqlite_outbox_row(row: &SqliteRow) -> Result<OutboxMessage, OutboxError> {
    let id: String = row
        .try_get("id")
        .map_err(|e| OutboxError::Repository(e.to_string()))?;
    let event_type: String = row
        .try_get("event_type")
        .map_err(|e| OutboxError::Repository(e.to_string()))?;
    let payload: Vec<u8> = row
        .try_get("payload")
        .map_err(|e| OutboxError::Repository(e.to_string()))?;
    let status_str: String = row
        .try_get("status")
        .map_err(|e| OutboxError::Repository(e.to_string()))?;
    let status = match status_str.as_str() {
        "pending" => OutboxStatus::Pending,
        "inflight" | "in_flight" => OutboxStatus::InFlight,
        "published" => OutboxStatus::Published,
        "failed" => OutboxStatus::Failed,
        _ => OutboxStatus::Pending,
    };
    let retry_count: i32 = row.try_get("retry_count").unwrap_or(0);
    let last_error: Option<String> = row.try_get("last_error").ok();
    let created_at: i64 = row
        .try_get("created_at")
        .map_err(|e| OutboxError::Repository(e.to_string()))?;
    let next_retry_at: Option<i64> = row.try_get("next_retry_at").ok();
    let lease_token: Option<String> = row.try_get("lease_token").ok();
    let lease_expires_at: Option<i64> = row.try_get("lease_expires_at").ok();
    let trace_context: Option<String> = row.try_get("trace_context").ok();

    Ok(OutboxMessage {
        id,
        event_type,
        payload,
        status,
        retry_count: retry_count as u32,
        last_error,
        created_at: created_at as u64,
        next_retry_at: next_retry_at.map(|v| v as u64),
        lease_token,
        lease_expires_at: lease_expires_at.map(|v| v as u64),
        trace_context,
    })
}

impl OutboxRepository for SqliteBackend {
    fn save(&self, msg: OutboxMessage) -> OutboxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move {
            let status = match msg.status {
                OutboxStatus::Pending => "pending",
                OutboxStatus::InFlight => "inflight",
                OutboxStatus::Published => "published",
                OutboxStatus::Failed => "failed",
            };
            sqlx::query(INSERT_OUTBOX_SQL)
                .bind(&msg.id)
                .bind(&msg.event_type)
                .bind(&msg.payload)
                .bind(status)
                .bind(msg.retry_count as i32)
                .bind(msg.created_at as i64)
                .bind(&msg.last_error)
                .bind(msg.next_retry_at.map(|v| v as i64))
                .bind(&msg.lease_token)
                .bind(msg.lease_expires_at.map(|v| v as i64))
                .bind(&msg.trace_context)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let rows = sqlx::query(OUTBOX_CLAIM_SQL)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut messages = Vec::with_capacity(rows.len());
            for row in &rows {
                messages.push(parse_sqlite_outbox_row(row)?);
            }
            Ok(messages)
        })
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let rows = sqlx::query(OUTBOX_CLAIM_SQL)
                .bind(limit as i64)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut claimed = Vec::with_capacity(rows.len());
            for row in &rows {
                let mut msg = parse_sqlite_outbox_row(row)?;
                let token = Uuid::new_v4().to_string();
                let updated = sqlx::query(SQLITE_OUTBOX_CLAIM_LEASE_SQL)
                    .bind(&token)
                    .bind(lease_duration_secs as i64)
                    .bind(&msg.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;

                let exp: i64 = updated.try_get(0).unwrap_or(0);
                msg.status = OutboxStatus::InFlight;
                msg.lease_token = Some(token);
                msg.lease_expires_at = Some(exp as u64);
                claimed.push(msg);
            }

            tx.commit()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(claimed)
        })
    }

    fn renew_outbox_lease(
        &self,
        id: &str,
        lease_token: &str,
        extension_secs: u64,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        let token_str = lease_token.to_string();
        Box::pin(async move {
            let res = sqlx::query(SQLITE_OUTBOX_RENEW_LEASE_SQL)
                .bind(extension_secs as i64)
                .bind(&id_str)
                .bind(&token_str)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(OutboxError::Repository(format!(
                    "cannot renew lease: message {id_str} not in flight, lease expired, or token mismatch"
                )));
            }

            Ok(())
        })
    }

    fn mark_published(
        &self,
        id: &str,
        lease_token: &str,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        let token_str = lease_token.to_string();
        Box::pin(async move {
            let res = sqlx::query(SQLITE_OUTBOX_MARK_PUBLISHED_SQL)
                .bind(&id_str)
                .bind(&token_str)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(OutboxError::Repository(format!(
                    "cannot mark published: message {id_str} not in flight, lease expired, or token mismatch"
                )));
            }
            Ok(())
        })
    }

    fn mark_failed(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        self.mark_failed_with_max_retries(id, lease_token, error, 5)
    }

    fn mark_failed_with_max_retries(
        &self,
        id: &str,
        lease_token: &str,
        error: String,
        max_retries: u32,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        let token_str = lease_token.to_string();
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_SELECT_RETRY_COUNT_SQL)
                .bind(&id_str)
                .bind(&token_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let Some(row) = row else {
                return Err(OutboxError::Repository(format!(
                    "cannot mark failed: message {id_str} not in flight, lease expired, or token mismatch"
                )));
            };

            let retry_count: i32 = row.try_get(0).unwrap_or(0) + 1;
            let status = if retry_count >= max_retries as i32 {
                "failed"
            } else {
                "pending"
            };
            let backoff_secs = 2u64.pow(retry_count.saturating_sub(1) as u32);

            let res = sqlx::query(SQLITE_OUTBOX_MARK_FAILED_SQL)
                .bind(status)
                .bind(retry_count)
                .bind(&error)
                .bind(backoff_secs as i64)
                .bind(&id_str)
                .bind(&token_str)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(OutboxError::Repository(format!(
                    "cannot mark failed: message {id_str} not in flight, lease expired, or token mismatch"
                )));
            }

            Ok(())
        })
    }

    fn recover_expired_claims(
        &self,
        _max_lease_age_secs: u64,
    ) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let res = sqlx::query(OUTBOX_RECOVER_SQL)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(res.rows_affected() as usize)
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_PENDING_COUNT_SQL)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_PUBLISHED_COUNT_SQL)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn failed_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_FAILED_COUNT_SQL)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn fetch_failed(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let rows = sqlx::query(SQLITE_OUTBOX_FETCH_FAILED_SQL)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut messages = Vec::with_capacity(rows.len());
            for row in rows {
                let id: String = row.try_get("id").unwrap_or_default();
                let event_type: String = row.try_get("event_type").unwrap_or_default();
                let payload: Vec<u8> = row.try_get("payload").unwrap_or_default();
                let retry_count: i32 = row.try_get("retry_count").unwrap_or(0);
                let created_at: i64 = row.try_get("created_at").unwrap_or(0);
                let last_error: Option<String> = row.try_get("last_error").ok();
                let next_retry_at: Option<i64> = row.try_get("next_retry_at").ok();
                let lease_token: Option<String> = row.try_get("lease_token").ok();
                let lease_expires_at: Option<i64> = row.try_get("lease_expires_at").ok();
                let trace_context: Option<String> = row.try_get("trace_context").ok();

                messages.push(OutboxMessage {
                    id,
                    event_type,
                    payload,
                    status: OutboxStatus::Failed,
                    retry_count: retry_count as u32,
                    created_at: created_at as u64,
                    last_error,
                    next_retry_at: next_retry_at.map(|v| v as u64),
                    lease_token,
                    lease_expires_at: lease_expires_at.map(|v| v as u64),
                    trace_context,
                });
            }
            Ok(messages)
        })
    }

    fn reprocess_failed(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let res = sqlx::query(SQLITE_OUTBOX_REPROCESS_FAILED_SQL)
                .bind(&id_str)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(OutboxError::Repository(format!(
                    "message {id_str} not found in failed status"
                )));
            }
            Ok(())
        })
    }

    fn purge_published(
        &self,
        older_than_secs: u64,
        limit: usize,
    ) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let res = sqlx::query(SQLITE_OUTBOX_PURGE_PUBLISHED_SQL)
                .bind(older_than_secs as i64)
                .bind(limit as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(res.rows_affected() as usize)
        })
    }
}

impl TransactionalOutbox<SqliteTransaction> for SqliteBackend {
    fn save_tx<'a>(
        &'a self,
        tx: &'a SqliteTransaction,
        msg: OutboxMessage,
    ) -> OutboxFuture<'a, Result<(), OutboxError>> {
        Box::pin(async move {
            let status = match msg.status {
                OutboxStatus::Pending => "pending",
                OutboxStatus::InFlight => "inflight",
                OutboxStatus::Published => "published",
                OutboxStatus::Failed => "failed",
            };
            let mut guard = tx.tx.lock().await;
            let active_tx = guard
                .as_mut()
                .ok_or_else(|| OutboxError::Repository("transaction already resolved".into()))?;

            sqlx::query(INSERT_OUTBOX_SQL)
                .bind(&msg.id)
                .bind(&msg.event_type)
                .bind(&msg.payload)
                .bind(status)
                .bind(msg.retry_count as i32)
                .bind(msg.created_at as i64)
                .bind(&msg.last_error)
                .bind(msg.next_retry_at.map(|v| v as i64))
                .bind(&msg.lease_token)
                .bind(msg.lease_expires_at.map(|v| v as i64))
                .bind(&msg.trace_context)
                .execute(&mut **active_tx)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

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
