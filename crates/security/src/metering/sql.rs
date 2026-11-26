//! SQL-backed [`UsageMeter`] across SQLite, PostgreSQL, and MySQL.

use std::sync::Arc;

use rume_data::persistence::{DatabaseBackend, DatabaseTransaction, DbValue};
use rume_data::sql;

use super::meter::{
    BillingSink, BoxFuture, MeteringError, UsageMeter, UsageRecord, current_epoch_secs,
};

const CURRENT_WINDOW: &str = "current";
const CURRENT_PERIOD_START: &str = "1970-01-01T00:00:00Z";
const MYSQL_CURRENT_PERIOD_START: &str = "1970-01-01 00:00:00";

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

fn period_start_for_dialect(dialect: &str) -> &'static str {
    // Why: MySQL 8.0+ strict mode rejects RFC 3339 `T`/`Z` literals in `DATETIME(6)`
    // columns (`RC-15`), so MySQL receives `"YYYY-MM-DD HH:MM:SS"` while Postgres/SQLite
    // keep the ISO-8601 UTC timestamp.
    match dialect {
        "mysql" => MYSQL_CURRENT_PERIOD_START,
        _ => CURRENT_PERIOD_START,
    }
}

fn counter_id(tenant_id: &str, metric: &str) -> String {
    // Why: length-prefixed framing (`<tlen>:<tid>:<mlen>:<metric>:<window>`)
    // prevents colon-split collisions such as `("a:b", "c")` vs `("a", "b:c")` (`RC-06`).
    format!(
        "{}:{tenant_id}:{}:{metric}:{CURRENT_WINDOW}",
        tenant_id.len(),
        metric.len()
    )
}

/// Persistent SQL [`UsageMeter`] across SQLite, PostgreSQL, and MySQL.
#[derive(Debug, Clone)]
pub struct SqlUsageMeter<B: DatabaseBackend> {
    backend: Arc<B>,
    flush_lock: Arc<tokio::sync::Mutex<()>>,
}

impl<B: DatabaseBackend> SqlUsageMeter<B> {
    /// Creates a new [`SqlUsageMeter`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            flush_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Ensures the `rume_usage_counters` schema exists on the underlying backend.
    pub async fn ensure_schema(&self) -> Result<(), MeteringError> {
        let ddl = select_sql(
            self.backend.dialect(),
            sql::PG_USAGE_METERING_SCHEMA,
            sql::MYSQL_USAGE_METERING_SCHEMA,
            sql::SQLITE_USAGE_METERING_SCHEMA,
        );
        self.backend
            .execute(ddl)
            .await
            .map(|_| ())
            .map_err(|e| MeteringError::Store(e.to_string()))
    }
}

impl<B: DatabaseBackend> UsageMeter for SqlUsageMeter<B> {
    fn record_usage<'a>(
        &'a self,
        tenant_id: &'a str,
        metric: &'a str,
        quantity: u64,
    ) -> BoxFuture<'a, Result<(), MeteringError>> {
        Box::pin(async move {
            if quantity == 0 {
                return Ok(());
            }
            let qty_i64 = i64::try_from(quantity)
                .map_err(|_| MeteringError::Store("usage quantity exceeds i64::MAX".into()))?;
            let id = counter_id(tenant_id, metric);
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_USAGE_METERING_INCREMENT,
                sql::MYSQL_USAGE_METERING_INCREMENT,
                sql::SQLITE_USAGE_METERING_INCREMENT,
            );
            let params = [
                DbValue::from(id),
                DbValue::from(tenant_id),
                DbValue::from(metric),
                DbValue::from(CURRENT_WINDOW),
                DbValue::from(qty_i64),
                DbValue::from(period_start_for_dialect(self.backend.dialect())),
            ];
            self.backend
                .execute_params(stmt, &params)
                .await
                .map(|_| ())
                .map_err(|e| MeteringError::Store(e.to_string()))
        })
    }

    fn get_usage<'a>(
        &'a self,
        tenant_id: &'a str,
        metric: &'a str,
    ) -> BoxFuture<'a, Result<u64, MeteringError>> {
        Box::pin(async move {
            let id = counter_id(tenant_id, metric);
            let stmt = select_sql(
                self.backend.dialect(),
                sql::PG_USAGE_METERING_GET,
                sql::MYSQL_USAGE_METERING_GET,
                sql::SQLITE_USAGE_METERING_GET,
            );
            let row = self
                .backend
                .fetch_optional(stmt, &[DbValue::from(id)])
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;
            match row {
                Some(r) => {
                    let count: i64 = r
                        .get("count")
                        .map_err(|e| MeteringError::Store(e.to_string()))?;
                    Ok(u64::try_from(count.max(0)).unwrap_or(0))
                }
                None => Ok(0),
            }
        })
    }

    fn flush<'a>(
        &'a self,
        sink: &'a dyn BillingSink,
    ) -> BoxFuture<'a, Result<usize, MeteringError>> {
        Box::pin(async move {
            // Why: serialize concurrent `flush()` calls on the same meter so two
            // tasks cannot snapshot and export the same un-decremented rows (`RC-15`).
            let _flush_guard = self.flush_lock.lock().await;

            let list_stmt = select_sql(
                self.backend.dialect(),
                sql::PG_USAGE_METERING_LIST_NONZERO,
                sql::MYSQL_USAGE_METERING_LIST_NONZERO,
                sql::SQLITE_USAGE_METERING_LIST_NONZERO,
            );
            let rows = self
                .backend
                .fetch_all(list_stmt, &[])
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;

            if rows.is_empty() {
                return Ok(0);
            }

            let now = current_epoch_secs();
            let mut ids_and_records = Vec::with_capacity(rows.len());
            for row in &rows {
                let id: String = row
                    .get("id")
                    .map_err(|e| MeteringError::Store(e.to_string()))?;
                let tenant_id: String = row
                    .get("tenant_id")
                    .map_err(|e| MeteringError::Store(e.to_string()))?;
                let metric: String = row
                    .get("metric")
                    .map_err(|e| MeteringError::Store(e.to_string()))?;
                let count: i64 = row
                    .get("count")
                    .map_err(|e| MeteringError::Store(e.to_string()))?;
                let quantity = u64::try_from(count.max(0)).unwrap_or(0);
                if quantity > 0 {
                    ids_and_records.push((
                        id,
                        count,
                        UsageRecord {
                            tenant_id,
                            metric,
                            quantity,
                            recorded_at: now,
                        },
                    ));
                }
            }

            if ids_and_records.is_empty() {
                return Ok(0);
            }

            let records: Vec<UsageRecord> =
                ids_and_records.iter().map(|(_, _, r)| r.clone()).collect();
            sink.export_batch(&records).await?;

            let dec_stmt = select_sql(
                self.backend.dialect(),
                sql::PG_USAGE_METERING_DECREMENT,
                sql::MYSQL_USAGE_METERING_DECREMENT,
                sql::SQLITE_USAGE_METERING_DECREMENT,
            );
            // Why: execute all checkpoint decrements atomically in a single transaction so
            // a partial failure mid-loop rolls back instead of leaving half-checkpointed
            // counters that would be partially double-billed on retry (`RC-15`).
            let tx = self
                .backend
                .begin_transaction()
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;
            for (id, count, _) in &ids_and_records {
                let params = [
                    DbValue::from(*count),
                    DbValue::from(*count),
                    DbValue::from(id.as_str()),
                ];
                if let Err(err) = tx.execute_params(dec_stmt, &params).await {
                    let _ = tx.rollback().await;
                    return Err(MeteringError::Store(err.to_string()));
                }
            }
            tx.commit()
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;

            Ok(records.len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_id_is_disjoint_for_colon_splits() {
        let left = counter_id("a:b", "c");
        let right = counter_id("a", "b:c");
        assert_ne!(
            left, right,
            "colon-containing tenant_id and metric splits must not collide"
        );
        assert_eq!(left, "3:a:b:1:c:current");
        assert_eq!(right, "1:a:3:b:c:current");
    }

    #[test]
    fn mysql_usage_metering_and_flags_sql_quote_window_and_widen_tenant_id() {
        assert!(
            sql::MYSQL_USAGE_METERING_SCHEMA.contains("`window`"),
            "MySQL usage_metering_schema.sql must backtick-quote reserved keyword `window`"
        );
        assert!(
            !sql::MYSQL_USAGE_METERING_SCHEMA.contains("VARCHAR(36)"),
            "MySQL usage_metering_schema.sql must widen tenant_id from VARCHAR(36) to VARCHAR(128)"
        );
        assert!(
            sql::MYSQL_USAGE_METERING_SCHEMA.contains("VARCHAR(128)"),
            "MySQL usage_metering_schema.sql must use VARCHAR(128) for tenant_id"
        );
        assert!(
            sql::MYSQL_USAGE_METERING_INCREMENT.contains("`window`"),
            "MySQL usage_metering_increment.sql must backtick-quote reserved keyword `window`"
        );
        assert!(
            !sql::MYSQL_FEATURE_FLAGS_SCHEMA.contains("VARCHAR(36)"),
            "MySQL feature_flags_schema.sql must widen tenant_id from VARCHAR(36) to VARCHAR(128)"
        );
        assert!(
            sql::MYSQL_FEATURE_FLAGS_SCHEMA.contains("VARCHAR(128)"),
            "MySQL feature_flags_schema.sql must use VARCHAR(128) for tenant_id"
        );
        assert_eq!(period_start_for_dialect("mysql"), "1970-01-01 00:00:00");
        assert_eq!(period_start_for_dialect("postgres"), "1970-01-01T00:00:00Z");
        assert_eq!(period_start_for_dialect("sqlite"), "1970-01-01T00:00:00Z");
    }
}
