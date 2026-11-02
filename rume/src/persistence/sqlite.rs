//! Embedded SQLite database backend integration using rusqlite.

use std::fmt::Debug;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::params;

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};
use crate::persistence::backend::{BoxFuture, DatabaseBackend, DatabaseTransaction};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;

/// Embedded SQLite database backend backed by rusqlite.
#[derive(Clone)]
pub struct SqliteBackend {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl Debug for SqliteBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteBackend").finish()
    }
}

impl SqliteBackend {
    /// Creates a new in-memory SQLite backend.
    pub fn in_memory() -> Result<Self, PersistenceError> {
        let conn = rusqlite::Connection::open_in_memory()
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        let backend = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        backend.configure_pragmas()?;
        Ok(backend)
    }

    /// Opens an SQLite database at the given file path.
    pub fn open(path: &str) -> Result<Self, PersistenceError> {
        let conn = rusqlite::Connection::open(path)
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        let backend = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        backend.configure_pragmas()?;
        Ok(backend)
    }

    /// Creates an SQLite backend from a [`DatabaseConfig`].
    pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let url = config.url.trim();
        if url == "sqlite::memory:" || url == ":memory:" || url == "memory://" {
            Self::in_memory()
        } else {
            let path = url.strip_prefix("sqlite://").unwrap_or(url);
            Self::open(path)
        }
    }

    fn configure_pragmas(&self) -> Result<(), PersistenceError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        Ok(())
    }

    /// Initializes the `rume_outbox` table if it does not already exist.
    pub async fn init_outbox_table(&self) -> Result<(), PersistenceError> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS rume_outbox (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                payload BLOB NOT NULL,
                status TEXT NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                last_error TEXT,
                next_retry_at INTEGER,
                lease_token TEXT,
                lease_expires_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_rume_outbox_status_retry ON rume_outbox (status, next_retry_at, created_at);",
        )
        .await
        .map(|_| ())
    }

    /// Initializes the `rume_migrations` table if it does not already exist.
    pub async fn init_migrations_table(&self) -> Result<(), PersistenceError> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS rume_migrations (
                version TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at INTEGER NOT NULL
            );",
        )
        .await
        .map(|_| ())
    }
}

impl DatabaseBackend for SqliteBackend {
    type Transaction = SqliteTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
                conn.query_row("SELECT 1", [], |_| Ok(()))
                    .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))
            })
            .await
            .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        let stmt = statement.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                conn.execute_batch(&stmt)
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                Ok(conn.changes())
            })
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        let q = query.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                let mut stmt = conn
                    .prepare(&q)
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                let mut rows = stmt
                    .query([])
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                if let Some(row) = rows
                    .next()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?
                {
                    let val: rusqlite::types::Value = row
                        .get(0)
                        .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                    let res = match val {
                        rusqlite::types::Value::Null => None,
                        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                        rusqlite::types::Value::Real(r) => Some(r.to_string()),
                        rusqlite::types::Value::Text(s) => Some(s),
                        rusqlite::types::Value::Blob(b) => {
                            Some(String::from_utf8_lossy(&b).to_string())
                        }
                    };
                    Ok(res)
                } else {
                    Ok(None)
                }
            })
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?
        })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let guard = conn
                    .lock()
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
                guard
                    .execute("BEGIN IMMEDIATE", [])
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
                drop(guard);
                Ok(SqliteTransaction {
                    conn,
                    status: Arc::new(AtomicU8::new(0)),
                })
            })
            .await
            .map_err(|e| PersistenceError::TransactionError(e.to_string()))?
        })
    }
}

/// An active SQLite transaction.
#[derive(Clone)]
pub struct SqliteTransaction {
    conn: Arc<Mutex<rusqlite::Connection>>,
    // 0: active, 1: committed, 2: rolled_back
    status: Arc<AtomicU8>,
}

impl Debug for SqliteTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTransaction")
            .field("status", &self.status.load(Ordering::Relaxed))
            .finish()
    }
}

impl DatabaseTransaction for SqliteTransaction {
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        let stmt = statement.to_string();
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction is already resolved".into(),
                ));
            }
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                conn.execute_batch(&stmt)
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                Ok(conn.changes())
            })
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        let q = query.to_string();
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction is already resolved".into(),
                ));
            }
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                let mut stmt = conn
                    .prepare(&q)
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                let mut rows = stmt
                    .query([])
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                if let Some(row) = rows
                    .next()
                    .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?
                {
                    let val: rusqlite::types::Value = row
                        .get(0)
                        .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
                    let res = match val {
                        rusqlite::types::Value::Null => None,
                        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                        rusqlite::types::Value::Real(r) => Some(r.to_string()),
                        rusqlite::types::Value::Text(s) => Some(s),
                        rusqlite::types::Value::Blob(b) => {
                            Some(String::from_utf8_lossy(&b).to_string())
                        }
                    };
                    Ok(res)
                } else {
                    Ok(None)
                }
            })
            .await
            .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?
        })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(PersistenceError::TransactionError(
                    "transaction is already resolved".into(),
                ));
            }
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
                conn.execute("COMMIT", [])
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
                Ok(())
            })
            .await
            .map_err(|e| PersistenceError::TransactionError(e.to_string()))?
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let conn = Arc::clone(&self.conn);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Ok(());
            }
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
                let _ = conn.execute("ROLLBACK", []);
                Ok(())
            })
            .await
            .map_err(|e| PersistenceError::TransactionError(e.to_string()))?
        })
    }
}

impl Drop for SqliteTransaction {
    fn drop(&mut self) {
        if Arc::strong_count(&self.status) <= 1 && self.status.load(Ordering::SeqCst) == 0 {
            let conn = Arc::clone(&self.conn);
            let status = Arc::clone(&self.status);
            if status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
                && let Ok(guard) = conn.lock()
            {
                let _ = guard.execute("ROLLBACK", []);
            }
        }
    }
}

impl OutboxRepository for SqliteBackend {
    fn save(&self, msg: OutboxMessage) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let status_str = match msg.status {
                    OutboxStatus::Pending => "pending",
                    OutboxStatus::InFlight => "inflight",
                    OutboxStatus::Published => "published",
                    OutboxStatus::Failed => "failed",
                };
                let retry_count = msg.retry_count as i64;
                let created_at = msg.created_at as i64;
                let next_retry_at = msg.next_retry_at.map(|t| t as i64);
                let lease_expires_at = msg.lease_expires_at.map(|t| t as i64);

                conn.execute(
                    "INSERT INTO rume_outbox (id, event_type, payload, status, retry_count, created_at, next_retry_at, lease_token, lease_expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        &msg.id,
                        &msg.event_type,
                        &msg.payload,
                        &status_str,
                        &retry_count,
                        &created_at,
                        &next_retry_at,
                        &msg.lease_token,
                        &lease_expires_at,
                    ],
                )
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(())
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let now = crate::outbox::message::current_timestamp() as i64;
                let mut stmt = conn
                    .prepare(
                        "SELECT id, event_type, payload, status, retry_count, created_at, last_error, next_retry_at, lease_token, lease_expires_at
                         FROM rume_outbox
                         WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= ?1)
                         ORDER BY created_at
                         LIMIT ?2",
                    )
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;

                let rows = stmt
                    .query_map(params![now, limit as i64], |row| {
                        let id: String = row.get(0)?;
                        let event_type: String = row.get(1)?;
                        let payload: Vec<u8> = row.get(2)?;
                        let status_str: String = row.get(3)?;
                        let retry_count: i64 = row.get(4)?;
                        let created_at: i64 = row.get(5)?;
                        let last_error: Option<String> = row.get(6)?;
                        let next_retry_at: Option<i64> = row.get(7)?;
                        let lease_token: Option<String> = row.get(8)?;
                        let lease_expires_at: Option<i64> = row.get(9)?;

                        let status = match status_str.as_str() {
                            "inflight" => OutboxStatus::InFlight,
                            "published" => OutboxStatus::Published,
                            "failed" => OutboxStatus::Failed,
                            _ => OutboxStatus::Pending,
                        };

                        Ok(OutboxMessage {
                            id,
                            event_type,
                            payload,
                            status,
                            retry_count: retry_count as u32,
                            created_at: created_at as u64,
                            last_error,
                            next_retry_at: next_retry_at.map(|t| t as u64),
                            lease_token,
                            lease_expires_at: lease_expires_at.map(|t| t as u64),
                        })
                    })
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;

                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| OutboxError::Repository(e.to_string()))?);
                }
                Ok(out)
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn claim_pending(
        &self,
        batch_size: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        self.claim_pending_with_lease(batch_size, 30)
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let now = crate::outbox::message::current_timestamp() as i64;
                let lease_expires_at = now + (lease_duration_secs as i64);
                let lease_token = uuid::Uuid::new_v4().to_string();

                conn.execute("BEGIN IMMEDIATE", [])
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;

                let claim_result: Result<Vec<OutboxMessage>, OutboxError> = (|| {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, event_type, payload, retry_count, created_at
                             FROM rume_outbox
                             WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= ?1)
                             ORDER BY created_at
                             LIMIT ?2",
                        )
                        .map_err(|e| OutboxError::Repository(e.to_string()))?;

                    let rows = stmt
                        .query_map(params![now, limit as i64], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Vec<u8>>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                            ))
                        })
                        .map_err(|e| OutboxError::Repository(e.to_string()))?;

                    let mut candidates = Vec::new();
                    for r in rows {
                        candidates.push(r.map_err(|e| OutboxError::Repository(e.to_string()))?);
                    }
                    drop(stmt);

                    let mut claimed = Vec::new();
                    for (id, event_type, payload, retry_count, created_at) in candidates {
                        conn.execute(
                            "UPDATE rume_outbox
                             SET status = 'inflight', lease_token = ?1, lease_expires_at = ?2
                             WHERE id = ?3 AND status = 'pending'",
                            params![&lease_token, lease_expires_at, &id],
                        )
                        .map_err(|e| OutboxError::Repository(e.to_string()))?;

                        claimed.push(OutboxMessage {
                            id,
                            event_type,
                            payload,
                            status: OutboxStatus::InFlight,
                            retry_count: retry_count as u32,
                            created_at: created_at as u64,
                            last_error: None,
                            next_retry_at: None,
                            lease_token: Some(lease_token.clone()),
                            lease_expires_at: Some(lease_expires_at as u64),
                        });
                    }

                    Ok(claimed)
                })();

                match claim_result {
                    Ok(msgs) => {
                        conn.execute("COMMIT", [])
                            .map_err(|e| OutboxError::Repository(e.to_string()))?;
                        Ok(msgs)
                    }
                    Err(err) => {
                        let _ = conn.execute("ROLLBACK", []);
                        Err(err)
                    }
                }
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn mark_published(&self, message_id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let conn = Arc::clone(&self.conn);
        let mid = message_id.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                conn.execute(
                    "UPDATE rume_outbox SET status = 'published', lease_token = NULL, lease_expires_at = NULL WHERE id = ?1",
                    params![&mid],
                )
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(())
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn mark_failed(
        &self,
        message_id: &str,
        error: String,
    ) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let conn = Arc::clone(&self.conn);
        let mid = message_id.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let now = crate::outbox::message::current_timestamp() as i64;
                conn.execute(
                    "UPDATE rume_outbox
                     SET retry_count = retry_count + 1,
                         status = CASE WHEN retry_count + 1 >= 5 THEN 'failed' ELSE 'pending' END,
                         last_error = ?1,
                         next_retry_at = CASE WHEN retry_count + 1 >= 5 THEN NULL ELSE ?2 + ((retry_count + 1) * 2) END,
                         lease_token = NULL,
                         lease_expires_at = NULL
                     WHERE id = ?3",
                    params![&error, now, &mid],
                )
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(())
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let now = crate::outbox::message::current_timestamp() as i64;
                let max_age = max_lease_age_secs as i64;

                let affected = conn.execute(
                    "UPDATE rume_outbox
                     SET status = 'pending', lease_token = NULL, lease_expires_at = NULL
                     WHERE status = 'inflight' AND (lease_expires_at <= ?1 OR created_at + ?2 <= ?1)",
                    params![now, max_age],
                )
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(affected)
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let count: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM rume_outbox WHERE status = 'pending' OR status = 'inflight'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(count as usize)
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        let conn = Arc::clone(&self.conn);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = conn
                    .lock()
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;
                let count: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM rume_outbox WHERE status = 'published'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(count as usize)
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }
}

impl TransactionalOutbox<SqliteTransaction> for SqliteBackend {
    fn save_tx<'a>(
        &'a self,
        tx: &'a SqliteTransaction,
        message: OutboxMessage,
    ) -> OutboxFuture<'a, Result<(), OutboxError>> {
        let conn = Arc::clone(&tx.conn);
        let status = Arc::clone(&tx.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(OutboxError::Repository(
                    "transaction is already resolved".into(),
                ));
            }
            tokio::task::spawn_blocking(move || {
                let conn = conn.lock().map_err(|e| OutboxError::Repository(e.to_string()))?;
                let status_str = match message.status {
                    OutboxStatus::Pending => "pending",
                    OutboxStatus::InFlight => "inflight",
                    OutboxStatus::Published => "published",
                    OutboxStatus::Failed => "failed",
                };
                let retry_count = message.retry_count as i64;
                let created_at = message.created_at as i64;
                let next_retry_at = message.next_retry_at.map(|t| t as i64);
                let lease_expires_at = message.lease_expires_at.map(|t| t as i64);

                conn.execute(
                    "INSERT INTO rume_outbox (id, event_type, payload, status, retry_count, created_at, next_retry_at, lease_token, lease_expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        &message.id,
                        &message.event_type,
                        &message.payload,
                        &status_str,
                        &retry_count,
                        &created_at,
                        &next_retry_at,
                        &message.lease_token,
                        &lease_expires_at,
                    ],
                )
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
                Ok(())
            })
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?
        })
    }
}
