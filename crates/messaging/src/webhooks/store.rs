//! Webhook delivery persistence (`InMemoryWebhookDeliveryStore`, `SqlWebhookDeliveryStore`).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rume_data::persistence::{DatabaseBackend, DatabaseTransaction, DbRow, DbValue};
use rume_data::sql;

use super::WebhookError;
use crate::outbox::repository::BoxFuture;

/// Lifecycle status of an outbound webhook delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookDeliveryStatus {
    /// Queued for initial delivery or scheduled for retry.
    Pending,
    /// Claimed by a dispatcher worker and currently in flight.
    InFlight,
    /// Successfully delivered (2xx HTTP response).
    Delivered,
    /// Permanently failed after exhausting retry attempts.
    Failed,
}

impl WebhookDeliveryStatus {
    /// Returns the database string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }

    /// Parses a database string representation.
    pub fn parse(s: &str) -> Result<Self, WebhookError> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_flight" | "inflight" => Ok(Self::InFlight),
            "delivered" => Ok(Self::Delivered),
            "failed" => Ok(Self::Failed),
            other => Err(WebhookError::Store(format!(
                "unknown webhook delivery status: {other}"
            ))),
        }
    }
}

/// Persisted log record for an outbound webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDeliveryRecord {
    /// Unique delivery identifier.
    pub id: String,
    /// Target [`super::WebhookEndpoint`] identifier.
    pub endpoint_id: String,
    /// Event type name.
    pub event_type: String,
    /// Serialized JSON payload body.
    pub payload: String,
    /// Current delivery status.
    pub status: WebhookDeliveryStatus,
    /// Number of delivery attempts made so far.
    pub attempt: u32,
    /// Scheduled timestamp for the next retry, if pending.
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// Summary of the most recent HTTP response or transport error.
    pub last_response: Option<String>,
    /// Record creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Record last-update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Storage contract for outbound webhook delivery logs.
pub trait WebhookDeliveryStore: std::fmt::Debug + Send + Sync + 'static {
    /// Inserts or updates a webhook delivery record.
    fn upsert(&self, record: WebhookDeliveryRecord) -> BoxFuture<'_, Result<(), WebhookError>>;

    /// Fetches a webhook delivery record by ID.
    fn get(&self, id: &str) -> BoxFuture<'_, Result<Option<WebhookDeliveryRecord>, WebhookError>>;

    /// Lists all delivery records for a given endpoint ID.
    fn list_by_endpoint(
        &self,
        endpoint_id: &str,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>>;

    /// Lists pending delivery records due on or before `now`, up to `limit`.
    fn list_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>>;

    /// Atomically claims up to `limit` due pending (or lease-expired `InFlight`) delivery records,
    /// transitioning them to [`WebhookDeliveryStatus::InFlight`] for `lease_duration`.
    fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        lease_duration: Duration,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>>;
}

/// In-memory [`WebhookDeliveryStore`] for tests and single-node development.
#[derive(Debug, Default)]
pub struct InMemoryWebhookDeliveryStore {
    records: RwLock<HashMap<String, WebhookDeliveryRecord>>,
}

impl InMemoryWebhookDeliveryStore {
    /// Creates a new empty [`InMemoryWebhookDeliveryStore`].
    pub fn new() -> Self {
        Self::default()
    }
}

impl WebhookDeliveryStore for InMemoryWebhookDeliveryStore {
    fn upsert(&self, record: WebhookDeliveryRecord) -> BoxFuture<'_, Result<(), WebhookError>> {
        Box::pin(async move {
            let mut guard = self
                .records
                .write()
                .map_err(|_| WebhookError::Store("webhook store lock poisoned".into()))?;
            guard.insert(record.id.clone(), record);
            Ok(())
        })
    }

    fn get(&self, id: &str) -> BoxFuture<'_, Result<Option<WebhookDeliveryRecord>, WebhookError>> {
        let id = id.to_string();
        Box::pin(async move {
            let guard = self
                .records
                .read()
                .map_err(|_| WebhookError::Store("webhook store lock poisoned".into()))?;
            Ok(guard.get(&id).cloned())
        })
    }

    fn list_by_endpoint(
        &self,
        endpoint_id: &str,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>> {
        let endpoint_id = endpoint_id.to_string();
        Box::pin(async move {
            let guard = self
                .records
                .read()
                .map_err(|_| WebhookError::Store("webhook store lock poisoned".into()))?;
            let mut items: Vec<WebhookDeliveryRecord> = guard
                .values()
                .filter(|r| r.endpoint_id == endpoint_id)
                .cloned()
                .collect();
            items.sort_by_key(|r| r.created_at);
            Ok(items)
        })
    }

    fn list_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>> {
        Box::pin(async move {
            let guard = self
                .records
                .read()
                .map_err(|_| WebhookError::Store("webhook store lock poisoned".into()))?;
            let mut items: Vec<WebhookDeliveryRecord> = guard
                .values()
                .filter(|r| {
                    r.status == WebhookDeliveryStatus::Pending
                        && r.next_attempt_at.is_none_or(|due| due <= now)
                })
                .cloned()
                .collect();
            items.sort_by_key(|r| r.created_at);
            items.truncate(limit);
            Ok(items)
        })
    }

    fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        lease_duration: Duration,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>> {
        Box::pin(async move {
            let mut guard = self
                .records
                .write()
                .map_err(|_| WebhookError::Store("webhook store lock poisoned".into()))?;
            let mut due_ids: Vec<(DateTime<Utc>, String)> = guard
                .values()
                .filter(|r| {
                    (r.status == WebhookDeliveryStatus::Pending
                        && r.next_attempt_at.is_none_or(|due| due <= now))
                        || (r.status == WebhookDeliveryStatus::InFlight
                            && r.next_attempt_at.is_some_and(|exp| exp <= now))
                })
                .map(|r| (r.created_at, r.id.clone()))
                .collect();
            due_ids.sort_by_key(|(created_at, _)| *created_at);
            due_ids.truncate(limit);

            let lease_delta = chrono::Duration::from_std(lease_duration).unwrap_or_default();
            let lease_until = now + lease_delta;
            let mut claimed = Vec::with_capacity(due_ids.len());
            for (_, id) in due_ids {
                if let Some(record) = guard.get_mut(&id) {
                    record.status = WebhookDeliveryStatus::InFlight;
                    record.next_attempt_at = Some(lease_until);
                    record.updated_at = now;
                    claimed.push(record.clone());
                }
            }
            Ok(claimed)
        })
    }
}

fn select_sql(
    dialect: &str,
    pg: &'static str,
    mysql: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match dialect {
        "postgres" => pg,
        "mysql" => mysql,
        _ => sqlite,
    }
}

fn format_ts(dialect: &str, dt: DateTime<Utc>) -> String {
    if dialect == "mysql" {
        dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
    } else {
        dt.to_rfc3339()
    }
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>, WebhookError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| WebhookError::Store(format!("invalid timestamp '{raw}': {e}")))
}

fn decode_delivery_row(row: &DbRow) -> Result<WebhookDeliveryRecord, WebhookError> {
    let map_err = |e: rume_data::persistence::PersistenceError| WebhookError::Store(e.to_string());
    let id: String = row.get("id").map_err(map_err)?;
    let endpoint_id: String = row.get("endpoint_id").map_err(map_err)?;
    let event_type: String = row.get("event_type").map_err(map_err)?;
    let payload: String = row.get("payload").map_err(map_err)?;
    let status_raw: String = row.get("status").map_err(map_err)?;
    let attempt_i64: i64 = row.get("attempt").map_err(map_err)?;
    let next_attempt_raw: Option<String> = row.get_opt("next_attempt_at").map_err(map_err)?;
    let last_response: Option<String> = row.get_opt("last_response").map_err(map_err)?;
    let created_raw: String = row.get("created_at").map_err(map_err)?;
    let updated_raw: String = row.get("updated_at").map_err(map_err)?;

    Ok(WebhookDeliveryRecord {
        id,
        endpoint_id,
        event_type,
        payload,
        status: WebhookDeliveryStatus::parse(&status_raw)?,
        attempt: u32::try_from(attempt_i64.max(0)).unwrap_or(u32::MAX),
        next_attempt_at: next_attempt_raw.as_deref().map(parse_ts).transpose()?,
        last_response,
        created_at: parse_ts(&created_raw)?,
        updated_at: parse_ts(&updated_raw)?,
    })
}

/// Persistent SQL [`WebhookDeliveryStore`] across SQLite, PostgreSQL, and MySQL.
#[derive(Debug, Clone)]
pub struct SqlWebhookDeliveryStore<B: DatabaseBackend> {
    backend: Arc<B>,
}

impl<B: DatabaseBackend> SqlWebhookDeliveryStore<B> {
    /// Creates a new [`SqlWebhookDeliveryStore`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// Ensures the `rume_webhook_deliveries` schema exists on the underlying backend.
    pub async fn ensure_schema(&self) -> Result<(), WebhookError> {
        let ddl = select_sql(
            self.backend.dialect(),
            sql::PG_WEBHOOK_DELIVERIES_SCHEMA,
            sql::MYSQL_WEBHOOK_DELIVERIES_SCHEMA,
            sql::SQLITE_WEBHOOK_DELIVERIES_SCHEMA,
        );
        self.backend
            .execute(ddl)
            .await
            .map(|_| ())
            .map_err(|e| WebhookError::Store(e.to_string()))
    }
}

impl<B: DatabaseBackend> WebhookDeliveryStore for SqlWebhookDeliveryStore<B> {
    fn upsert(&self, record: WebhookDeliveryRecord) -> BoxFuture<'_, Result<(), WebhookError>> {
        Box::pin(async move {
            let dialect = self.backend.dialect();
            let stmt = select_sql(
                dialect,
                sql::PG_WEBHOOK_DELIVERIES_UPSERT,
                sql::MYSQL_WEBHOOK_DELIVERIES_UPSERT,
                sql::SQLITE_WEBHOOK_DELIVERIES_UPSERT,
            );
            let next_attempt_val = match record.next_attempt_at {
                Some(dt) => DbValue::Text(format_ts(dialect, dt)),
                None => DbValue::Null,
            };
            let last_response_val = match record.last_response {
                Some(s) => DbValue::Text(s),
                None => DbValue::Null,
            };
            let params = [
                DbValue::Text(record.id),
                DbValue::Text(record.endpoint_id),
                DbValue::Text(record.event_type),
                DbValue::Text(record.payload),
                DbValue::Text(record.status.as_str().to_string()),
                DbValue::Int(i64::from(record.attempt)),
                next_attempt_val,
                last_response_val,
                DbValue::Text(format_ts(dialect, record.created_at)),
                DbValue::Text(format_ts(dialect, record.updated_at)),
            ];
            self.backend
                .execute_params(stmt, &params)
                .await
                .map(|_| ())
                .map_err(|e| WebhookError::Store(e.to_string()))
        })
    }

    fn get(&self, id: &str) -> BoxFuture<'_, Result<Option<WebhookDeliveryRecord>, WebhookError>> {
        let id = id.to_string();
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_WEBHOOK_DELIVERIES_GET,
                sql::MYSQL_WEBHOOK_DELIVERIES_GET,
                sql::SQLITE_WEBHOOK_DELIVERIES_GET,
            );
            let row = self
                .backend
                .fetch_optional(stmt, &[DbValue::Text(id)])
                .await
                .map_err(|e| WebhookError::Store(e.to_string()))?;
            row.as_ref().map(decode_delivery_row).transpose()
        })
    }

    fn list_by_endpoint(
        &self,
        endpoint_id: &str,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>> {
        let endpoint_id = endpoint_id.to_string();
        Box::pin(async move {
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_WEBHOOK_DELIVERIES_LIST_BY_ENDPOINT,
                sql::MYSQL_WEBHOOK_DELIVERIES_LIST_BY_ENDPOINT,
                sql::SQLITE_WEBHOOK_DELIVERIES_LIST_BY_ENDPOINT,
            );
            let rows = self
                .backend
                .fetch_all(stmt, &[DbValue::Text(endpoint_id)])
                .await
                .map_err(|e| WebhookError::Store(e.to_string()))?;
            rows.iter().map(decode_delivery_row).collect()
        })
    }

    fn list_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>> {
        Box::pin(async move {
            let dialect = self.backend.dialect();
            let stmt = select_sql(
                dialect,
                sql::PG_WEBHOOK_DELIVERIES_LIST_DUE,
                sql::MYSQL_WEBHOOK_DELIVERIES_LIST_DUE,
                sql::SQLITE_WEBHOOK_DELIVERIES_LIST_DUE,
            );
            let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = self
                .backend
                .fetch_all(
                    stmt,
                    &[
                        DbValue::Text(format_ts(dialect, now)),
                        DbValue::Int(limit_i64),
                    ],
                )
                .await
                .map_err(|e| WebhookError::Store(e.to_string()))?;
            rows.iter().map(decode_delivery_row).collect()
        })
    }

    fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        lease_duration: Duration,
    ) -> BoxFuture<'_, Result<Vec<WebhookDeliveryRecord>, WebhookError>> {
        Box::pin(async move {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let dialect = self.backend.dialect();
            let lease_delta = chrono::Duration::from_std(lease_duration).unwrap_or_default();
            let lease_until = now + lease_delta;
            let now_str = format_ts(dialect, now);
            let lease_until_str = format_ts(dialect, lease_until);
            let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

            if dialect == "mysql" {
                let tx = self
                    .backend
                    .begin_transaction()
                    .await
                    .map_err(|e| WebhookError::Store(e.to_string()))?;
                let rows = tx
                    .fetch_all(
                        sql::MYSQL_WEBHOOK_DELIVERIES_CLAIM_DUE_SELECT,
                        &[
                            DbValue::Text(now_str.clone()),
                            DbValue::Text(now_str.clone()),
                            DbValue::Int(limit_i64),
                        ],
                    )
                    .await
                    .map_err(|e| WebhookError::Store(e.to_string()))?;
                let mut claimed = Vec::with_capacity(rows.len());
                for row in &rows {
                    let mut record = decode_delivery_row(row)?;
                    tx.execute_params(
                        sql::MYSQL_WEBHOOK_DELIVERIES_CLAIM_DUE_UPDATE,
                        &[
                            DbValue::Text(lease_until_str.clone()),
                            DbValue::Text(now_str.clone()),
                            DbValue::Text(record.id.clone()),
                        ],
                    )
                    .await
                    .map_err(|e| WebhookError::Store(e.to_string()))?;
                    record.status = WebhookDeliveryStatus::InFlight;
                    record.next_attempt_at = Some(lease_until);
                    record.updated_at = now;
                    claimed.push(record);
                }
                tx.commit()
                    .await
                    .map_err(|e| WebhookError::Store(e.to_string()))?;
                return Ok(claimed);
            }

            let stmt = if dialect == "postgres" {
                sql::PG_WEBHOOK_DELIVERIES_CLAIM_DUE
            } else {
                sql::SQLITE_WEBHOOK_DELIVERIES_CLAIM_DUE
            };
            let rows = self
                .backend
                .fetch_all(
                    stmt,
                    &[
                        DbValue::Text(now_str),
                        DbValue::Text(lease_until_str),
                        DbValue::Int(limit_i64),
                    ],
                )
                .await
                .map_err(|e| WebhookError::Store(e.to_string()))?;
            let mut claimed: Vec<WebhookDeliveryRecord> = rows
                .iter()
                .map(decode_delivery_row)
                .collect::<Result<_, _>>()?;
            claimed.sort_by_key(|r| r.created_at);
            Ok(claimed)
        })
    }
}
