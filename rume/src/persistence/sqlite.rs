//! Embedded SQLite database backend integration using SQLx.

use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::outbox::message::current_timestamp;
use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};
use crate::persistence::backend::{BoxFuture, DatabaseBackend, DatabaseTransaction};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;

const OUTBOX_SCHEMA: &str = include_str!("sql/sqlite/outbox_schema.sql");
const OUTBOX_CLAIM_SQL: &str = include_str!("sql/sqlite/outbox_claim.sql");
const MIGRATIONS_SCHEMA: &str = include_str!("sql/sqlite/migrations_schema.sql");
const INSERT_OUTBOX_SQL: &str = include_str!("sql/sqlite/outbox_insert.sql");
const OUTBOX_RECOVER_SQL: &str = include_str!("sql/sqlite/outbox_recover.sql");

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
        let url = if path.starts_with("sqlite://") || path.starts_with("sqlite:") {
            path.to_string()
        } else {
            format!("sqlite://{path}")
        };
        let opts = SqliteConnectOptions::from_str(&url)
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_millis(5000));

        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(opts)
            .await
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

        Ok(Self { pool })
    }

    /// Creates an SQLite backend from a [`DatabaseConfig`].
    pub async fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let url = config.url.trim();
        if url == "sqlite::memory:" || url == ":memory:" || url == "memory://" || url.is_empty() {
            Self::in_memory().await
        } else {
            Self::open(url).await
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
            sqlx::query("SELECT 1")
                .execute(&self.pool)
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            Ok(())
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        Box::pin(async move {
            let res = sqlx::query(&stmt)
                .execute(&self.pool)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(res.rows_affected())
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.trim().to_string();
        Box::pin(async move {
            let row: Option<SqliteRow> = sqlx::query(&q)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;

            Ok(row.and_then(|r| extract_sqlite_scalar(&r)))
        })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        Box::pin(async move {
            let tx = self
                .pool
                .begin()
                .await
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            Ok(SqliteTransaction {
                tx: Arc::new(tokio::sync::Mutex::new(Some(tx))),
            })
        })
    }
}

/// Active transaction handle for SQLite.
#[derive(Clone)]
pub struct SqliteTransaction {
    tx: Arc<tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Sqlite>>>>,
}

impl Debug for SqliteTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTransaction").finish()
    }
}

impl DatabaseTransaction for SqliteTransaction {
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard
                .as_mut()
                .ok_or_else(|| PersistenceError::TransactionError("transaction already resolved".into()))?;
            let res = sqlx::query(&stmt)
                .execute(&mut **tx)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(res.rows_affected())
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.trim().to_string();
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard
                .as_mut()
                .ok_or_else(|| PersistenceError::TransactionError("transaction already resolved".into()))?;
            let row: Option<SqliteRow> = sqlx::query(&q)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(row.and_then(|r| extract_sqlite_scalar(&r)))
        })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let tx_arc = Arc::clone(&self.tx);
        Box::pin(async move {
            let mut guard = tx_arc.lock().await;
            let tx = guard
                .take()
                .ok_or_else(|| PersistenceError::TransactionError("transaction already resolved".into()))?;
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
    let id: String = row.try_get("id").map_err(|e| OutboxError::Repository(e.to_string()))?;
    let event_type: String = row.try_get("event_type").map_err(|e| OutboxError::Repository(e.to_string()))?;
    let payload: Vec<u8> = row.try_get("payload").map_err(|e| OutboxError::Repository(e.to_string()))?;
    let status_str: String = row.try_get("status").map_err(|e| OutboxError::Repository(e.to_string()))?;
    let status = match status_str.as_str() {
        "pending" => OutboxStatus::Pending,
        "inflight" | "in_flight" => OutboxStatus::InFlight,
        "published" => OutboxStatus::Published,
        "failed" => OutboxStatus::Failed,
        _ => OutboxStatus::Pending,
    };
    let retry_count: i32 = row.try_get("retry_count").unwrap_or(0);
    let last_error: Option<String> = row.try_get("last_error").ok();
    let created_at: i64 = row.try_get("created_at").map_err(|e| OutboxError::Repository(e.to_string()))?;
    let next_retry_at: Option<i64> = row.try_get("next_retry_at").ok();
    let lease_token: Option<String> = row.try_get("lease_token").ok();
    let lease_expires_at: Option<i64> = row.try_get("lease_expires_at").ok();

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
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn fetch_pending(&self, limit: usize) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let now = current_timestamp();
            let rows = sqlx::query(OUTBOX_CLAIM_SQL)
                .bind(now as i64)
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
            let now = current_timestamp();
            let expires_at = now + lease_duration_secs;
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let rows = sqlx::query(OUTBOX_CLAIM_SQL)
                .bind(now as i64)
                .bind(limit as i64)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut claimed = Vec::with_capacity(rows.len());
            for row in &rows {
                let mut msg = parse_sqlite_outbox_row(row)?;
                let token = Uuid::new_v4().to_string();
                sqlx::query(
                    "UPDATE rume_outbox SET status = 'inflight', lease_token = ?1, lease_expires_at = ?2 WHERE id = ?3"
                )
                .bind(&token)
                .bind(expires_at as i64)
                .bind(&msg.id)
                .execute(&mut *tx)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

                msg.status = OutboxStatus::InFlight;
                msg.lease_token = Some(token);
                msg.lease_expires_at = Some(expires_at);
                claimed.push(msg);
            }

            tx.commit()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(claimed)
        })
    }

    fn mark_published(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            sqlx::query(
                "UPDATE rume_outbox SET status = 'published', lease_token = NULL, lease_expires_at = NULL WHERE id = ?1"
            )
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn mark_failed(&self, id: &str, error: String) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let now = current_timestamp();
            let row = sqlx::query("SELECT retry_count FROM rume_outbox WHERE id = ?1")
                .bind(&id_str)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let retry_count: i32 = row.and_then(|r| r.try_get(0).ok()).unwrap_or(0) + 1;
            let status = if retry_count >= 5 { "failed" } else { "pending" };
            let backoff_secs = 2u64.pow(retry_count.saturating_sub(1) as u32);
            let next_retry = now + backoff_secs;

            sqlx::query(
                "UPDATE rume_outbox SET status = ?1, retry_count = ?2, last_error = ?3, next_retry_at = ?4, lease_token = NULL, lease_expires_at = NULL WHERE id = ?5"
            )
            .bind(status)
            .bind(retry_count)
            .bind(&error)
            .bind(next_retry as i64)
            .bind(&id_str)
            .execute(&self.pool)
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(())
        })
    }

    fn recover_expired_claims(&self, max_lease_age_secs: u64) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let now = current_timestamp();
            let res = sqlx::query(OUTBOX_RECOVER_SQL)
                .bind(now as i64)
                .bind(max_lease_age_secs as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(res.rows_affected() as usize)
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT COUNT(*) FROM rume_outbox WHERE status = 'pending' OR status = 'inflight'"
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT COUNT(*) FROM rume_outbox WHERE status = 'published'"
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
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
                .execute(&mut **active_tx)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(())
        })
    }
}
