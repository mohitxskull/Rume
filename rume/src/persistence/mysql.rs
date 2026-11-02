//! Production MySQL database backend integration using mysql_async.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, Row, Value};
use tokio::sync::Mutex;

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};
use crate::persistence::backend::{BoxFuture, DatabaseBackend, DatabaseTransaction};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;

/// Production MySQL database backend backed by a `mysql_async` connection pool.
#[derive(Clone)]
pub struct MysqlBackend {
    pool: Pool,
}

impl Debug for MysqlBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlBackend").finish()
    }
}

impl MysqlBackend {
    /// Creates a new MySQL backend from connection configuration.
    pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let opts = Opts::from_url(&config.url)
            .map_err(|e| PersistenceError::ConnectionFailed(format!("invalid mysql url: {e}")))?;

        let pool_opts = PoolOpts::default().with_constraints(
            PoolConstraints::new(1, config.max_connections.max(1) as usize).unwrap_or_default(),
        );

        let opts_builder = OptsBuilder::from_opts(opts).pool_opts(pool_opts);
        let pool = Pool::new(opts_builder);
        Ok(Self { pool })
    }

    /// Creates a backend directly from an existing `mysql_async::Pool`.
    pub fn from_pool(pool: Pool) -> Self {
        Self { pool }
    }

    /// Access the underlying `mysql_async::Pool`.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Schema DDL for the transactional outbox table in MySQL.
    pub const OUTBOX_TABLE_SCHEMA: &'static str = r#"
CREATE TABLE IF NOT EXISTS rume_outbox (
    id VARCHAR(64) PRIMARY KEY,
    event_type VARCHAR(255) NOT NULL,
    payload LONGBLOB NOT NULL,
    status VARCHAR(32) NOT NULL,
    retry_count INT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    last_error TEXT,
    next_retry_at BIGINT,
    lease_token VARCHAR(64),
    lease_expires_at BIGINT,
    INDEX idx_rume_outbox_claim (status, created_at)
) ENGINE=InnoDB;
"#;
}

fn row_to_scalar(row: Option<Row>) -> Option<String> {
    let row = row?;
    let val: Option<Value> = row.get(0);
    match val {
        Some(Value::Bytes(bytes)) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Some(Value::Int(i)) => Some(i.to_string()),
        Some(Value::UInt(u)) => Some(u.to_string()),
        Some(Value::Float(f)) => Some(f.to_string()),
        Some(Value::Double(d)) => Some(d.to_string()),
        Some(Value::Date(y, m, d, h, min, s, u)) => {
            Some(format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}:{s:02}.{u:06}"))
        }
        Some(Value::Time(neg, d, h, min, s, u)) => {
            Some(format!("{}{d}d {h:02}:{min:02}:{s:02}.{u:06}", if neg { "-" } else { "" }))
        }
        Some(Value::NULL) | None => None,
    }
}

impl DatabaseBackend for MysqlBackend {
    type Transaction = MysqlTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            conn.query_drop("SELECT 1")
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            Ok(())
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let pool = self.pool.clone();
        let stmt = statement.to_string();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            conn.query_drop(&stmt)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(conn.affected_rows())
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let pool = self.pool.clone();
        let q = query.to_string();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            let row: Option<Row> = conn
                .query_first(&q)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(row_to_scalar(row))
        })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            conn.query_drop("START TRANSACTION")
                .await
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            Ok(MysqlTransaction {
                conn: Arc::new(Mutex::new(Some(conn))),
                status: Arc::new(AtomicU8::new(0)),
            })
        })
    }
}

/// Active MySQL transaction handle.
#[derive(Clone)]
pub struct MysqlTransaction {
    pub(crate) conn: Arc<Mutex<Option<Conn>>>,
    pub(crate) status: Arc<AtomicU8>, // 0: active, 1: committed, 2: rolled_back
}

impl Debug for MysqlTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlTransaction")
            .field("status", &self.status.load(Ordering::SeqCst))
            .finish()
    }
}

impl DatabaseTransaction for MysqlTransaction {
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        let conn_lock = Arc::clone(&self.conn);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }
            let mut guard = conn_lock.lock().await;
            let conn = guard
                .as_mut()
                .ok_or_else(|| PersistenceError::TransactionError("connection dropped".into()))?;
            conn.query_drop(&stmt)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(conn.affected_rows())
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.to_string();
        let conn_lock = Arc::clone(&self.conn);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }
            let mut guard = conn_lock.lock().await;
            let conn = guard
                .as_mut()
                .ok_or_else(|| PersistenceError::TransactionError("connection dropped".into()))?;
            let row: Option<Row> = conn
                .query_first(&q)
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(row_to_scalar(row))
        })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let conn_lock = Arc::clone(&self.conn);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }
            let mut guard = conn_lock.lock().await;
            if let Some(conn) = guard.as_mut() {
                conn.query_drop("COMMIT")
                    .await
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            }
            Ok(())
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let conn_lock = Arc::clone(&self.conn);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }
            let mut guard = conn_lock.lock().await;
            if let Some(conn) = guard.as_mut() {
                conn.query_drop("ROLLBACK")
                    .await
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            }
            Ok(())
        })
    }
}

impl Drop for MysqlTransaction {
    fn drop(&mut self) {
        if self.status.load(Ordering::SeqCst) == 0 {
            let conn_lock = Arc::clone(&self.conn);
            let status = Arc::clone(&self.status);
            status.store(2, Ordering::SeqCst);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let mut guard = conn_lock.lock().await;
                    if let Some(mut conn) = guard.take() {
                        let _ = conn.query_drop("ROLLBACK").await;
                    }
                });
            }
        }
    }
}

fn parse_mysql_outbox_row(row: Row) -> Result<OutboxMessage, OutboxError> {
    let get_val = |idx: usize| -> Option<Value> {
        row.get_opt::<Value, _>(idx).and_then(|r| r.ok())
    };

    let id = match get_val(0) {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(&b).to_string(),
        _ => return Err(OutboxError::Repository("missing id".into())),
    };

    let event_type = match get_val(1) {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(&b).to_string(),
        _ => return Err(OutboxError::Repository("missing event_type".into())),
    };

    let payload = match get_val(2) {
        Some(Value::Bytes(b)) => b,
        _ => return Err(OutboxError::Repository("missing payload".into())),
    };

    let status_str = match get_val(3) {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(&b).to_string(),
        _ => return Err(OutboxError::Repository("missing status".into())),
    };

    let retry_count = match get_val(4) {
        Some(Value::Int(i)) => i as u32,
        Some(Value::UInt(u)) => u as u32,
        _ => 0,
    };

    let created_at = match get_val(5) {
        Some(Value::Int(i)) => i as u64,
        Some(Value::UInt(u)) => u,
        _ => return Err(OutboxError::Repository("missing created_at".into())),
    };

    let last_error = match get_val(6) {
        Some(Value::Bytes(b)) => Some(String::from_utf8_lossy(&b).to_string()),
        _ => None,
    };

    let next_retry_at = match get_val(7) {
        Some(Value::Int(i)) => Some(i as u64),
        Some(Value::UInt(u)) => Some(u),
        _ => None,
    };

    let lease_token = match get_val(8) {
        Some(Value::Bytes(b)) => Some(String::from_utf8_lossy(&b).to_string()),
        _ => None,
    };

    let lease_expires_at = match get_val(9) {
        Some(Value::Int(i)) => Some(i as u64),
        Some(Value::UInt(u)) => Some(u),
        _ => None,
    };

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
        retry_count,
        created_at,
        last_error,
        next_retry_at,
        lease_token,
        lease_expires_at,
    })
}

impl OutboxRepository for MysqlBackend {
    fn save(&self, message: OutboxMessage) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let status_str = match message.status {
                OutboxStatus::Pending => "pending",
                OutboxStatus::InFlight => "inflight",
                OutboxStatus::Published => "published",
                OutboxStatus::Failed => "failed",
            };

            let stmt = r#"
INSERT INTO rume_outbox
    (id, event_type, payload, status, retry_count, created_at, next_retry_at, lease_token, lease_expires_at)
VALUES
    (:id, :event_type, :payload, :status, :retry_count, :created_at, :next_retry_at, :lease_token, :lease_expires_at)
"#;
            conn.exec_drop(
                stmt,
                params! {
                    "id" => message.id,
                    "event_type" => message.event_type,
                    "payload" => message.payload,
                    "status" => status_str,
                    "retry_count" => message.retry_count,
                    "created_at" => message.created_at,
                    "next_retry_at" => message.next_retry_at,
                    "lease_token" => message.lease_token,
                    "lease_expires_at" => message.lease_expires_at,
                },
            )
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(())
        })
    }

    fn fetch_pending(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;

            let stmt = format!(
                r#"
SELECT id, event_type, payload, status, retry_count, created_at, last_error, next_retry_at, lease_token, lease_expires_at
FROM rume_outbox
WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= :now)
ORDER BY created_at ASC
LIMIT {limit}
"#
            );
            let rows: Vec<Row> = conn
                .exec(
                    stmt,
                    params! {
                        "now" => now,
                    },
                )
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            rows.into_iter().map(parse_mysql_outbox_row).collect()
        })
    }

    fn claim_pending(
        &self,
        limit: usize,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        self.claim_pending_with_lease(limit, 30)
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let lease_token = uuid::Uuid::new_v4().to_string();
            let lease_expires_at = now + lease_duration_secs as i64;

            conn.query_drop("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            conn.query_drop("START TRANSACTION")
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let select_sql = format!(
                r#"
SELECT id, event_type, payload, status, retry_count, created_at, last_error, next_retry_at, lease_token, lease_expires_at
FROM rume_outbox
WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= :now)
LIMIT {limit}
FOR UPDATE SKIP LOCKED
"#
            );
            let rows: Vec<Row> = conn
                .exec(
                    select_sql,
                    params! {
                        "now" => now,
                    },
                )
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut claimed = Vec::new();
            for row in rows {
                let mut msg = parse_mysql_outbox_row(row)?;
                msg.status = OutboxStatus::InFlight;
                msg.lease_token = Some(lease_token.clone());
                msg.lease_expires_at = Some(lease_expires_at as u64);

                let update_sql = r#"
UPDATE rume_outbox
SET status = 'inflight', lease_token = :lease_token, lease_expires_at = :lease_expires_at
WHERE id = :id
"#;
                conn.exec_drop(
                    update_sql,
                    params! {
                        "lease_token" => &lease_token,
                        "lease_expires_at" => lease_expires_at,
                        "id" => &msg.id,
                    },
                )
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

                claimed.push(msg);
            }

            conn.query_drop("COMMIT")
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(claimed)
        })
    }

    fn mark_published(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let pool = self.pool.clone();
        let id_str = id.to_string();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            conn.exec_drop(
                "UPDATE rume_outbox SET status = 'published', lease_token = NULL, lease_expires_at = NULL WHERE id = :id",
                params! { "id" => id_str },
            )
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn mark_failed(&self, id: &str, error: String) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let pool = self.pool.clone();
        let id_str = id.to_string();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let next_retry = now + 5;
            let stmt = r#"
UPDATE rume_outbox
SET retry_count = retry_count + 1,
    last_error = :err,
    next_retry_at = :next_retry,
    lease_token = NULL,
    lease_expires_at = NULL,
    status = CASE WHEN retry_count + 1 >= 5 THEN 'failed' ELSE 'pending' END
WHERE id = :id
"#;
            conn.exec_drop(
                stmt,
                params! {
                    "err" => error,
                    "next_retry" => next_retry,
                    "id" => id_str,
                },
            )
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let max_age = max_lease_age_secs as i64;

            let stmt = r#"
UPDATE rume_outbox
SET status = 'pending', lease_token = NULL, lease_expires_at = NULL
WHERE status = 'inflight' AND (lease_expires_at <= :now OR created_at + :max_age <= :now)
"#;
            conn.exec_drop(
                stmt,
                params! {
                    "now" => now,
                    "max_age" => max_age,
                },
            )
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(conn.affected_rows() as usize)
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let row: Option<Row> = conn
                .query_first("SELECT count(*) FROM rume_outbox WHERE status = 'pending'")
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let count: i64 = row
                .and_then(|r| r.get(0))
                .unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut conn = pool
                .get_conn()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let row: Option<Row> = conn
                .query_first("SELECT count(*) FROM rume_outbox WHERE status = 'published'")
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let count: i64 = row
                .and_then(|r| r.get(0))
                .unwrap_or(0);
            Ok(count as usize)
        })
    }
}

impl TransactionalOutbox<MysqlTransaction> for MysqlBackend {
    fn save_tx<'a>(
        &'a self,
        tx: &'a MysqlTransaction,
        message: OutboxMessage,
    ) -> OutboxFuture<'a, Result<(), OutboxError>> {
        Box::pin(async move {
            let stmt = r#"
INSERT INTO rume_outbox
    (id, event_type, payload, status, retry_count, created_at, next_retry_at, lease_token, lease_expires_at)
VALUES
    (:id, :event_type, :payload, :status, :retry_count, :created_at, :next_retry_at, :lease_token, :lease_expires_at)
"#;
            let mut guard = tx.conn.lock().await;
            let conn = guard
                .as_mut()
                .ok_or_else(|| OutboxError::Repository("connection dropped".into()))?;

            let status_str = match message.status {
                OutboxStatus::Pending => "pending",
                OutboxStatus::InFlight => "inflight",
                OutboxStatus::Published => "published",
                OutboxStatus::Failed => "failed",
            };

            conn.exec_drop(
                stmt,
                params! {
                    "id" => message.id,
                    "event_type" => message.event_type,
                    "payload" => message.payload,
                    "status" => status_str,
                    "retry_count" => message.retry_count,
                    "created_at" => message.created_at,
                    "next_retry_at" => message.next_retry_at,
                    "lease_token" => message.lease_token,
                    "lease_expires_at" => message.lease_expires_at,
                },
            )
            .await
            .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(())
        })
    }
}
