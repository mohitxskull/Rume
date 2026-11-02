//! Production PostgreSQL database backend integration using deadpool-postgres and tokio-postgres.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use deadpool_postgres::{Config as PoolConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio::sync::Mutex;
use tokio_postgres::NoTls;

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};
use crate::persistence::backend::{BoxFuture, DatabaseBackend, DatabaseTransaction};
use crate::persistence::config::DatabaseConfig;
use crate::persistence::error::PersistenceError;

/// Production PostgreSQL database backend backed by a deadpool-managed connection pool.
#[derive(Clone)]
pub struct PostgresBackend {
    pool: Pool,
}

impl Debug for PostgresBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresBackend")
            .field("status", &self.pool.status())
            .finish()
    }
}

impl PostgresBackend {
    /// Creates a new PostgreSQL backend from connection configuration.
    pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
        let pool_config = PoolConfig {
            url: Some(config.url.clone()),
            manager: Some(ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            }),
            ..Default::default()
        };

        let pool = pool_config
            .builder(NoTls)
            .map_err(|e| {
                PersistenceError::ConnectionFailed(format!("failed to configure pool: {e}"))
            })?
            .max_size(config.max_connections.max(1) as usize)
            .runtime(Runtime::Tokio1)
            .build()
            .map_err(|e| {
                PersistenceError::ConnectionFailed(format!("failed to initialize pool: {e}"))
            })?;

        Ok(Self { pool })
    }

    /// Creates a backend directly from an existing deadpool-postgres pool.
    pub fn from_pool(pool: Pool) -> Self {
        Self { pool }
    }

    /// Access the underlying `deadpool_postgres::Pool`.
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Schema DDL for the transactional outbox table in PostgreSQL.
    pub const OUTBOX_TABLE_SCHEMA: &'static str = r#"
CREATE TABLE IF NOT EXISTS rume_outbox (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload BYTEA NOT NULL,
    status TEXT NOT NULL,
    retry_count INT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    last_error TEXT,
    next_retry_at BIGINT,
    lease_token TEXT,
    lease_expires_at BIGINT
);
"#;

    /// Initializes the `rume_outbox` table if it does not already exist.
    pub async fn init_outbox_table(&self) -> Result<(), PersistenceError> {
        self.execute(Self::OUTBOX_TABLE_SCHEMA).await?;
        Ok(())
    }
}

impl DatabaseBackend for PostgresBackend {
    type Transaction = PostgresTransaction;

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            client
                .simple_query("SELECT 1")
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            Ok(())
        })
    }

    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            let affected = client
                .execute(&stmt, &[])
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(affected)
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            let rows = client
                .query(&q, &[])
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            if let Some(row) = rows.first() {
                let val: Option<String> = row.try_get(0).ok();
                Ok(val)
            } else {
                Ok(None)
            }
        })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
            client
                .execute("BEGIN", &[])
                .await
                .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;

            Ok(PostgresTransaction {
                client: Arc::new(Mutex::new(Some(client))),
                status: Arc::new(AtomicU8::new(0)), // 0 = Active
            })
        })
    }
}

/// Active transaction on a dedicated pooled PostgreSQL connection.
#[derive(Debug, Clone)]
pub struct PostgresTransaction {
    client: Arc<Mutex<Option<deadpool_postgres::Client>>>,
    status: Arc<AtomicU8>, // 0: Active, 1: Committed, 2: RolledBack
}

impl DatabaseTransaction for PostgresTransaction {
    fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let stmt = statement.to_string();
        let client_lock = Arc::clone(&self.client);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }
            let mut guard = client_lock.lock().await;
            let client = guard
                .as_mut()
                .ok_or_else(|| PersistenceError::TransactionError("connection dropped".into()))?;
            let affected = client
                .execute(&stmt, &[])
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            Ok(affected)
        })
    }

    fn query_scalar(&self, query: &str) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let q = query.to_string();
        let client_lock = Arc::clone(&self.client);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status.load(Ordering::SeqCst) != 0 {
                return Err(PersistenceError::TransactionError(
                    "transaction already resolved".into(),
                ));
            }
            let mut guard = client_lock.lock().await;
            let client = guard
                .as_mut()
                .ok_or_else(|| PersistenceError::TransactionError("connection dropped".into()))?;
            let rows = client
                .query(&q, &[])
                .await
                .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
            if let Some(row) = rows.first() {
                let val: Option<String> = row.try_get(0).ok();
                Ok(val)
            } else {
                Ok(None)
            }
        })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let client_lock = Arc::clone(&self.client);
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
            let mut guard = client_lock.lock().await;
            if let Some(client) = guard.as_mut() {
                client
                    .execute("COMMIT", &[])
                    .await
                    .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
            }
            Ok(())
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        let client_lock = Arc::clone(&self.client);
        let status = Arc::clone(&self.status);
        Box::pin(async move {
            if status
                .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Ok(()); // already resolved
            }
            let mut guard = client_lock.lock().await;
            if let Some(client) = guard.as_mut() {
                let _ = client.execute("ROLLBACK", &[]).await;
            }
            Ok(())
        })
    }
}

impl Drop for PostgresTransaction {
    fn drop(&mut self) {
        if Arc::strong_count(&self.status) <= 1 && self.status.load(Ordering::SeqCst) == 0 {
            let client_lock = Arc::clone(&self.client);
            let status = Arc::clone(&self.status);
            tokio::spawn(async move {
                if status
                    .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let mut guard = client_lock.lock().await;
                    if let Some(client) = guard.as_mut() {
                        let _ = client.execute("ROLLBACK", &[]).await;
                    }
                }
            });
        }
    }
}

impl OutboxRepository for PostgresBackend {
    fn save(&self, msg: OutboxMessage) -> OutboxFuture<'_, Result<(), OutboxError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let stmt = "INSERT INTO rume_outbox (id, event_type, payload, status, retry_count, created_at, next_retry_at, lease_token, lease_expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)";
            let status_str = match msg.status {
                OutboxStatus::Pending => "pending",
                OutboxStatus::InFlight => "inflight",
                OutboxStatus::Published => "published",
                OutboxStatus::Failed => "failed",
            };
            let retry_count = msg.retry_count as i32;
            let created_at = msg.created_at as i64;
            let next_retry_at = msg.next_retry_at.map(|t| t as i64);
            let lease_expires_at = msg.lease_expires_at.map(|t| t as i64);

            client
                .execute(
                    stmt,
                    &[
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
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let stmt = "SELECT id, event_type, payload, status, retry_count, created_at, last_error, next_retry_at, lease_token, lease_expires_at FROM rume_outbox WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= $1) ORDER BY created_at LIMIT $2";
            let rows = client
                .query(stmt, &[&now, &(limit as i64)])
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut out = Vec::new();
            for r in rows {
                let id: String = r.get(0);
                let event_type: String = r.get(1);
                let payload: Vec<u8> = r.get(2);
                let status_str: String = r.get(3);
                let retry_count: i32 = r.get(4);
                let created_at: i64 = r.get(5);
                let last_error: Option<String> = r.get(6);
                let next_retry_at: Option<i64> = r.get(7);
                let lease_token: Option<String> = r.get(8);
                let lease_expires_at: Option<i64> = r.get(9);

                let status = match status_str.as_str() {
                    "inflight" => OutboxStatus::InFlight,
                    "published" => OutboxStatus::Published,
                    "failed" => OutboxStatus::Failed,
                    _ => OutboxStatus::Pending,
                };

                out.push(OutboxMessage {
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
                });
            }
            Ok(out)
        })
    }

    fn claim_pending_with_lease(
        &self,
        limit: usize,
        lease_duration_secs: u64,
    ) -> OutboxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let lease_exp =
                (crate::outbox::message::current_timestamp() + lease_duration_secs) as i64;
            let token = uuid::Uuid::new_v4().to_string();

            let stmt = "UPDATE rume_outbox SET status = 'inflight', lease_token = $1, lease_expires_at = $2 WHERE id IN (SELECT id FROM rume_outbox WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= $3) ORDER BY created_at LIMIT $4 FOR UPDATE SKIP LOCKED) RETURNING id, event_type, payload, status, retry_count, created_at, last_error, next_retry_at, lease_token, lease_expires_at";
            let rows = client
                .query(stmt, &[&token, &lease_exp, &now, &(limit as i64)])
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let mut out = Vec::new();
            for r in rows {
                let id: String = r.get(0);
                let event_type: String = r.get(1);
                let payload: Vec<u8> = r.get(2);
                let retry_count: i32 = r.get(4);
                let created_at: i64 = r.get(5);
                let last_error: Option<String> = r.get(6);
                let next_retry_at: Option<i64> = r.get(7);
                let lease_token: Option<String> = r.get(8);
                let lease_expires_at: Option<i64> = r.get(9);

                out.push(OutboxMessage {
                    id,
                    event_type,
                    payload,
                    status: OutboxStatus::InFlight,
                    retry_count: retry_count as u32,
                    created_at: created_at as u64,
                    last_error,
                    next_retry_at: next_retry_at.map(|t| t as u64),
                    lease_token,
                    lease_expires_at: lease_expires_at.map(|t| t as u64),
                });
            }
            Ok(out)
        })
    }

    fn mark_published(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let stmt = "UPDATE rume_outbox SET status = 'published', last_error = NULL, next_retry_at = NULL, lease_token = NULL, lease_expires_at = NULL WHERE id = $1";
            client
                .execute(stmt, &[&id_str])
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn mark_failed(&self, id: &str, error: String) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let backoff_secs = 2i64;
            let next_retry = now + backoff_secs;

            let stmt = "UPDATE rume_outbox SET retry_count = retry_count + 1, last_error = $1, next_retry_at = $2, lease_token = NULL, lease_expires_at = NULL, status = CASE WHEN retry_count + 1 >= 5 THEN 'failed' ELSE 'pending' END WHERE id = $3";
            client
                .execute(stmt, &[&error, &next_retry, &id_str])
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(())
        })
    }

    fn recover_expired_claims(
        &self,
        max_lease_age_secs: u64,
    ) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let now = crate::outbox::message::current_timestamp() as i64;
            let max_age = max_lease_age_secs as i64;

            let stmt = "UPDATE rume_outbox SET status = 'pending', lease_token = NULL, lease_expires_at = NULL WHERE status = 'inflight' AND (lease_expires_at <= $1 OR created_at + $2 <= $1)";
            let affected = client
                .execute(stmt, &[&now, &max_age])
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            Ok(affected as usize)
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let row = client
                .query_one(
                    "SELECT count(*) FROM rume_outbox WHERE status = 'pending' OR status = 'inflight'",
                    &[],
                )
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let count: i64 = row.get(0);
            Ok(count as usize)
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let client = self
                .pool
                .get()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let row = client
                .query_one(
                    "SELECT count(*) FROM rume_outbox WHERE status = 'published'",
                    &[],
                )
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let count: i64 = row.get(0);
            Ok(count as usize)
        })
    }
}

impl TransactionalOutbox<PostgresTransaction> for PostgresBackend {
    fn save_tx<'a>(
        &'a self,
        tx: &'a PostgresTransaction,
        message: OutboxMessage,
    ) -> OutboxFuture<'a, Result<(), OutboxError>> {
        Box::pin(async move {
            let stmt = "INSERT INTO rume_outbox (id, event_type, payload, status, retry_count, created_at, next_retry_at, lease_token, lease_expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)";
            let mut guard = tx.client.lock().await;
            let client = guard
                .as_mut()
                .ok_or_else(|| OutboxError::Repository("connection dropped".into()))?;

            let status_str = match message.status {
                OutboxStatus::Pending => "pending",
                OutboxStatus::InFlight => "inflight",
                OutboxStatus::Published => "published",
                OutboxStatus::Failed => "failed",
            };
            let retry_count = message.retry_count as i32;
            let created_at = message.created_at as i64;
            let next_retry_at = message.next_retry_at.map(|t| t as i64);
            let lease_expires_at = message.lease_expires_at.map(|t| t as i64);

            client
                .execute(
                    stmt,
                    &[
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
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(())
        })
    }
}
