//! SQLite [`OutboxRepository`] and [`TransactionalOutbox`] implementations.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use rume_data::persistence::sqlite::{SqliteBackend, SqliteTransaction};

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};

const OUTBOX_CLAIM_SQL: &str = include_str!("sql/sqlite/outbox_claim.sql");
const INSERT_OUTBOX_SQL: &str = include_str!("sql/sqlite/outbox_insert.sql");
const OUTBOX_RECOVER_SQL: &str = include_str!("sql/sqlite/outbox_recover.sql");
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
                .execute(self.pool())
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
                .fetch_all(self.pool())
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
            // Why: Acquire the SQLite write lock up front with `BEGIN IMMEDIATE` instead of
            // deferred `pool().begin()` + `SELECT` + `UPDATE`. Under deferred `BEGIN` (even in
            // WAL mode), `SELECT` takes a `SHARED` snapshot lock and the subsequent `UPDATE`
            // attempts a `SHARED -> RESERVED` lock upgrade; if another writer committed since
            // our snapshot began, SQLite rejects the upgrade immediately with
            // `SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT` without waiting on `busy_timeout`.
            // `BEGIN IMMEDIATE` queues on `busy_timeout` before starting the read snapshot.
            // We also keep the `SELECT` + per-row `UPDATE` loop rather than collapsing to a
            // single `UPDATE ... RETURNING` because each claimed row requires a distinct
            // `Uuid::new_v4()` lease token generated per row. Revisit if SQLite gains a
            // built-in per-row random UUID expression or lease tokens become batch-scoped.
            let mut tx = self
                .pool()
                .begin_with("BEGIN IMMEDIATE")
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
                // Why: Use `fetch_optional` rather than `fetch_one` so a row whose state
                // changed outside this query's `status = 'pending'` predicate is skipped
                // defensively instead of aborting the whole batch with `RowNotFound`.
                let Some(updated) = sqlx::query(SQLITE_OUTBOX_CLAIM_LEASE_SQL)
                    .bind(&token)
                    .bind(lease_duration_secs as i64)
                    .bind(&msg.id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| OutboxError::Repository(e.to_string()))?
                else {
                    continue;
                };

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
                .execute(self.pool())
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
                .execute(self.pool())
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
                .fetch_optional(self.pool())
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
                .execute(self.pool())
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
                .execute(self.pool())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(res.rows_affected() as usize)
        })
    }

    fn pending_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_PENDING_COUNT_SQL)
                .fetch_one(self.pool())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_PUBLISHED_COUNT_SQL)
                .fetch_one(self.pool())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn failed_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(SQLITE_OUTBOX_FAILED_COUNT_SQL)
                .fetch_one(self.pool())
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
                .fetch_all(self.pool())
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
                .execute(self.pool())
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
                .execute(self.pool())
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
            let mut guard = tx.raw_tx().lock().await;
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

#[cfg(test)]
mod sqlite_outbox_tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rume_data::persistence::DatabaseConfig;
    use tokio::sync::Barrier;

    use super::*;

    struct TempSqliteDir {
        path: PathBuf,
    }

    impl TempSqliteDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rume-sqlite-outbox-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create temp sqlite dir");
            Self { path }
        }
    }

    impl Drop for TempSqliteDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sqlite_concurrent_outbox_claims_with_immediate_tx_never_hit_busy_or_duplicate() {
        let temp_dir = TempSqliteDir::new();
        let db_path = temp_dir.path.join("outbox.db");
        let config = DatabaseConfig::new(db_path.display().to_string()).with_max_connections(8);
        let backend = Arc::new(
            SqliteBackend::new(&config)
                .await
                .expect("open multi-connection sqlite backend"),
        );
        backend
            .init_outbox_table()
            .await
            .expect("init outbox table");

        const TOTAL_MESSAGES: usize = 40;
        const WORKER_COUNT: usize = 8;

        for i in 0..TOTAL_MESSAGES {
            let msg = OutboxMessage::new(
                format!("msg-{i:03}"),
                "order.created",
                format!(r#"{{"seq":{i}}}"#).into_bytes(),
            );
            backend.save(msg).await.expect("save outbox message");
        }

        let barrier = Arc::new(Barrier::new(WORKER_COUNT));
        let mut handles = Vec::with_capacity(WORKER_COUNT);
        for _ in 0..WORKER_COUNT {
            let backend = Arc::clone(&backend);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let mut claimed_ids = Vec::new();
                loop {
                    let batch = backend.claim_pending_with_lease(2, 30).await?;
                    if batch.is_empty() {
                        break;
                    }
                    for msg in batch {
                        assert_eq!(msg.status, OutboxStatus::InFlight);
                        assert!(msg.lease_token.is_some());
                        assert!(msg.lease_expires_at.is_some());
                        claimed_ids.push(msg.id);
                    }
                }
                Ok::<Vec<String>, OutboxError>(claimed_ids)
            }));
        }

        let mut all_claimed = Vec::with_capacity(TOTAL_MESSAGES);
        let mut errors = Vec::new();
        for handle in handles {
            match handle.await.expect("worker task must not panic") {
                Ok(ids) => all_claimed.extend(ids),
                Err(err) => errors.push(err.to_string()),
            }
        }

        assert!(
            errors.is_empty(),
            "concurrent claims must not hit SQLITE_BUSY or RowNotFound, got: {errors:?}"
        );
        assert_eq!(
            all_claimed.len(),
            TOTAL_MESSAGES,
            "all seeded messages must be claimed across workers"
        );
        let unique: HashSet<String> = all_claimed.into_iter().collect();
        assert_eq!(
            unique.len(),
            TOTAL_MESSAGES,
            "claimed message IDs must be strictly disjoint across workers"
        );
        assert!(
            backend
                .fetch_pending(TOTAL_MESSAGES)
                .await
                .expect("fetch_pending")
                .is_empty(),
            "no messages should remain in pending status after all are claimed"
        );
        // `pending_count` includes both `'pending'` and `'inflight'` rows until published.
        assert_eq!(
            backend.pending_count().await.expect("pending_count"),
            TOTAL_MESSAGES
        );
    }
}
