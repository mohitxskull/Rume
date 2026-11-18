//! MySQL [`OutboxRepository`] and [`TransactionalOutbox`] implementations.

use sqlx::mysql::MySqlRow;
use sqlx::{Acquire, Row};
use uuid::Uuid;

use rume_data::persistence::mysql::{MysqlBackend, MysqlTransaction};

use crate::outbox::{
    BoxFuture as OutboxFuture, OutboxError, OutboxMessage, OutboxRepository, OutboxStatus,
    TransactionalOutbox,
};

const MYSQL_SET_ISOLATION_LEVEL_SQL: &str =
    include_str!("../../../data/src/persistence/sql/mysql/set_isolation_level.sql");
const OUTBOX_CLAIM_SQL: &str = include_str!("sql/mysql/outbox_claim.sql");
const INSERT_OUTBOX_SQL: &str = include_str!("sql/mysql/outbox_insert.sql");
const OUTBOX_RECOVER_SQL: &str = include_str!("sql/mysql/outbox_recover.sql");
const MYSQL_OUTBOX_CLAIM_LEASE_SQL: &str = include_str!("sql/mysql/outbox_claim_lease.sql");
const MYSQL_OUTBOX_SELECT_LEASE_EXPIRES_AT_SQL: &str =
    include_str!("sql/mysql/outbox_select_lease_expires_at.sql");
const MYSQL_OUTBOX_RENEW_LEASE_SQL: &str = include_str!("sql/mysql/outbox_renew_lease.sql");
const MYSQL_OUTBOX_MARK_PUBLISHED_SQL: &str = include_str!("sql/mysql/outbox_mark_published.sql");
const MYSQL_OUTBOX_SELECT_RETRY_COUNT_SQL: &str =
    include_str!("sql/mysql/outbox_select_retry_count.sql");
const MYSQL_OUTBOX_MARK_FAILED_SQL: &str = include_str!("sql/mysql/outbox_mark_failed.sql");
const MYSQL_OUTBOX_PENDING_COUNT_SQL: &str = include_str!("sql/mysql/outbox_pending_count.sql");
const MYSQL_OUTBOX_PUBLISHED_COUNT_SQL: &str = include_str!("sql/mysql/outbox_published_count.sql");
const MYSQL_OUTBOX_FAILED_COUNT_SQL: &str = include_str!("sql/mysql/outbox_failed_count.sql");
const MYSQL_OUTBOX_FETCH_FAILED_SQL: &str = include_str!("sql/mysql/outbox_fetch_failed.sql");
const MYSQL_OUTBOX_REPROCESS_FAILED_SQL: &str =
    include_str!("sql/mysql/outbox_reprocess_failed.sql");
const MYSQL_OUTBOX_PURGE_PUBLISHED_SQL: &str = include_str!("sql/mysql/outbox_purge_published.sql");

fn parse_mysql_outbox_row(row: &MySqlRow) -> Result<OutboxMessage, OutboxError> {
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
    let created_at: u64 = row
        .try_get("created_at")
        .map_err(|e| OutboxError::Repository(e.to_string()))?;
    let next_retry_at: Option<u64> = row.try_get("next_retry_at").ok();
    let lease_token: Option<String> = row.try_get("lease_token").ok();
    let lease_expires_at: Option<u64> = row.try_get("lease_expires_at").ok();
    let trace_context: Option<String> = row.try_get("trace_context").ok();

    Ok(OutboxMessage {
        id,
        event_type,
        payload,
        status,
        retry_count: retry_count as u32,
        last_error,
        created_at,
        next_retry_at,
        lease_token,
        lease_expires_at,
        trace_context,
    })
}

impl OutboxRepository for MysqlBackend {
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
                .bind(msg.created_at)
                .bind(&msg.last_error)
                .bind(msg.next_retry_at)
                .bind(&msg.lease_token)
                .bind(msg.lease_expires_at)
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
                messages.push(parse_mysql_outbox_row(row)?);
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
            let mut conn = self
                .pool()
                .acquire()
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            sqlx::query(MYSQL_SET_ISOLATION_LEVEL_SQL)
                .execute(&mut *conn)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;
            let mut tx = conn
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
                let mut msg = parse_mysql_outbox_row(row)?;
                let token = Uuid::new_v4().to_string();
                sqlx::query(MYSQL_OUTBOX_CLAIM_LEASE_SQL)
                    .bind(&token)
                    .bind(lease_duration_secs as i64)
                    .bind(&msg.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;

                // NOTE: `lease_expires_at` is a MySQL `BIGINT UNSIGNED` column; decode into `u64` (not `i64`).
                let exp: u64 = sqlx::query_scalar(MYSQL_OUTBOX_SELECT_LEASE_EXPIRES_AT_SQL)
                    .bind(&msg.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| OutboxError::Repository(e.to_string()))?;

                msg.status = OutboxStatus::InFlight;
                msg.lease_token = Some(token);
                msg.lease_expires_at = Some(exp);
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
            let res = sqlx::query(MYSQL_OUTBOX_RENEW_LEASE_SQL)
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
            let res = sqlx::query(MYSQL_OUTBOX_MARK_PUBLISHED_SQL)
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
            let row = sqlx::query(MYSQL_OUTBOX_SELECT_RETRY_COUNT_SQL)
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

            let res = sqlx::query(MYSQL_OUTBOX_MARK_FAILED_SQL)
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
            let row = sqlx::query(MYSQL_OUTBOX_PENDING_COUNT_SQL)
                .fetch_one(self.pool())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn published_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(MYSQL_OUTBOX_PUBLISHED_COUNT_SQL)
                .fetch_one(self.pool())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            let count: i64 = row.try_get(0).unwrap_or(0);
            Ok(count as usize)
        })
    }

    fn failed_count(&self) -> OutboxFuture<'_, Result<usize, OutboxError>> {
        Box::pin(async move {
            let row = sqlx::query(MYSQL_OUTBOX_FAILED_COUNT_SQL)
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
            let rows = sqlx::query(MYSQL_OUTBOX_FETCH_FAILED_SQL)
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
                let created_at: u64 = row.try_get("created_at").unwrap_or(0);
                let last_error: Option<String> = row.try_get("last_error").ok();
                let next_retry_at: Option<u64> = row.try_get("next_retry_at").ok();
                let lease_token: Option<String> = row.try_get("lease_token").ok();
                let lease_expires_at: Option<u64> = row.try_get("lease_expires_at").ok();
                let trace_context: Option<String> = row.try_get("trace_context").ok();

                messages.push(OutboxMessage {
                    id,
                    event_type,
                    payload,
                    status: OutboxStatus::Failed,
                    retry_count: retry_count as u32,
                    created_at,
                    last_error,
                    next_retry_at,
                    lease_token,
                    lease_expires_at,
                    trace_context,
                });
            }
            Ok(messages)
        })
    }

    fn reprocess_failed(&self, id: &str) -> OutboxFuture<'_, Result<(), OutboxError>> {
        let id_str = id.to_string();
        Box::pin(async move {
            let res = sqlx::query(MYSQL_OUTBOX_REPROCESS_FAILED_SQL)
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
            let res = sqlx::query(MYSQL_OUTBOX_PURGE_PUBLISHED_SQL)
                .bind(older_than_secs as i64)
                .bind(limit as i64)
                .execute(self.pool())
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(res.rows_affected() as usize)
        })
    }
}

impl TransactionalOutbox<MysqlTransaction> for MysqlBackend {
    fn save_tx<'a>(
        &'a self,
        tx: &'a MysqlTransaction,
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
                .bind(msg.created_at)
                .bind(&msg.last_error)
                .bind(msg.next_retry_at)
                .bind(&msg.lease_token)
                .bind(msg.lease_expires_at)
                .bind(&msg.trace_context)
                .execute(&mut **active_tx)
                .await
                .map_err(|e| OutboxError::Repository(e.to_string()))?;

            Ok(())
        })
    }
}
