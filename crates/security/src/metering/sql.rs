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
}

impl<B: DatabaseBackend> SqlUsageMeter<B> {
    /// Creates a new [`SqlUsageMeter`] backed by `backend`.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
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
            // Why: serialize concurrent `flush()` calls across replicas at the database
            // layer rather than with an in-process mutex. `begin_immediate_transaction()`
            // acquires SQLite's RESERVED write lock up front (`BEGIN IMMEDIATE`), while
            // Postgres/MySQL lock the snapshotted counter rows via `SELECT ... FOR UPDATE`
            // in deterministic `(tenant_id, metric)` order. Rolling back on sink failure
            // preserves all counts without double-billing or losing concurrent increments.
            let tx = self
                .backend
                .begin_immediate_transaction()
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;

            let list_stmt = select_sql(
                self.backend.dialect(),
                sql::PG_USAGE_METERING_LIST_NONZERO,
                sql::MYSQL_USAGE_METERING_LIST_NONZERO,
                sql::SQLITE_USAGE_METERING_LIST_NONZERO,
            );
            let rows = match tx.fetch_all(list_stmt, &[]).await {
                Ok(rows) => rows,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(MeteringError::Store(e.to_string()));
                }
            };

            if rows.is_empty() {
                let _ = tx.rollback().await;
                return Ok(0);
            }

            let now = current_epoch_secs();
            let mut ids_and_records = Vec::with_capacity(rows.len());
            for row in &rows {
                let parsed = (|| -> Result<(String, String, String, i64), MeteringError> {
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
                    Ok((id, tenant_id, metric, count))
                })();
                let (id, tenant_id, metric, count) = match parsed {
                    Ok(v) => v,
                    Err(err) => {
                        let _ = tx.rollback().await;
                        return Err(err);
                    }
                };
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
                let _ = tx.rollback().await;
                return Ok(0);
            }

            let records: Vec<UsageRecord> =
                ids_and_records.iter().map(|(_, _, r)| r.clone()).collect();
            if let Err(err) = sink.export_batch(&records).await {
                let _ = tx.rollback().await;
                return Err(err);
            }

            let dec_stmt = select_sql(
                self.backend.dialect(),
                sql::PG_USAGE_METERING_DECREMENT,
                sql::MYSQL_USAGE_METERING_DECREMENT,
                sql::SQLITE_USAGE_METERING_DECREMENT,
            );
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

    #[test]
    fn postgres_and_sqlite_usage_metering_sql_double_quote_window_keyword() {
        assert!(
            sql::PG_USAGE_METERING_SCHEMA.contains("\"window\""),
            "PostgreSQL usage_metering_schema.sql must double-quote reserved keyword \"window\""
        );
        assert_eq!(
            sql::PG_USAGE_METERING_SCHEMA.matches("\"window\"").count(),
            2,
            "PostgreSQL usage_metering_schema.sql must double-quote \"window\" in both column def and unique index"
        );
        assert!(
            sql::PG_USAGE_METERING_INCREMENT.contains("\"window\""),
            "PostgreSQL usage_metering_increment.sql must double-quote reserved keyword \"window\""
        );
        assert!(
            sql::SQLITE_USAGE_METERING_SCHEMA.contains("\"window\""),
            "SQLite usage_metering_schema.sql must double-quote \"window\""
        );
        assert_eq!(
            sql::SQLITE_USAGE_METERING_SCHEMA
                .matches("\"window\"")
                .count(),
            2,
            "SQLite usage_metering_schema.sql must double-quote \"window\" in both column def and unique index"
        );
        assert!(
            sql::SQLITE_USAGE_METERING_INCREMENT.contains("\"window\""),
            "SQLite usage_metering_increment.sql must double-quote \"window\""
        );
    }

    #[tokio::test]
    async fn concurrent_independent_sql_usage_meters_serialize_flush_without_double_billing() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        use rume_data::persistence::{DatabaseConfig, SqliteBackend};
        use tokio::sync::Barrier;

        struct DelayedSink {
            exported: Mutex<Vec<UsageRecord>>,
            fail_next: AtomicBool,
            delay: Duration,
        }

        impl BillingSink for DelayedSink {
            fn export_batch<'a>(
                &'a self,
                records: &'a [UsageRecord],
            ) -> BoxFuture<'a, Result<(), MeteringError>> {
                let records = records.to_vec();
                Box::pin(async move {
                    if !self.delay.is_zero() {
                        tokio::time::sleep(self.delay).await;
                    }
                    if self.fail_next.swap(false, Ordering::SeqCst) {
                        return Err(MeteringError::BillingSink(
                            "transient billing sink error".into(),
                        ));
                    }
                    self.exported.lock().unwrap().extend(records);
                    Ok(())
                })
            }
        }

        let db_path = std::env::temp_dir().join(format!(
            "rume-metering-rc12-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let cfg = DatabaseConfig::new(db_path.to_string_lossy().as_ref()).with_max_connections(4);
        let backend = Arc::new(SqliteBackend::new(&cfg).await.expect("open sqlite"));

        let meter_a = SqlUsageMeter::new(Arc::clone(&backend));
        let meter_b = SqlUsageMeter::new(Arc::clone(&backend));
        meter_a.ensure_schema().await.expect("ensure_schema");

        // Record initial usage.
        meter_a
            .record_usage("tenant-1", "api_calls", 10)
            .await
            .expect("record 10");
        meter_b
            .record_usage("tenant-1", "api_calls", 5)
            .await
            .expect("record 5");

        // Two independent SqlUsageMeter instances flush concurrently across a Barrier.
        let sink = Arc::new(DelayedSink {
            exported: Mutex::new(Vec::new()),
            fail_next: AtomicBool::new(false),
            delay: Duration::from_millis(30),
        });
        let barrier = Arc::new(Barrier::new(2));

        let m_a = meter_a.clone();
        let s_a = Arc::clone(&sink);
        let b_a = Arc::clone(&barrier);
        let t_a = tokio::spawn(async move {
            b_a.wait().await;
            m_a.flush(s_a.as_ref()).await
        });

        let m_b = meter_b.clone();
        let s_b = Arc::clone(&sink);
        let b_b = Arc::clone(&barrier);
        let t_b = tokio::spawn(async move {
            b_b.wait().await;
            m_b.flush(s_b.as_ref()).await
        });

        // Concurrent increment while flush is in flight.
        let m_inc = meter_a.clone();
        let t_inc = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            m_inc.record_usage("tenant-1", "api_calls", 7).await
        });

        t_a.await.unwrap().expect("flush a");
        t_b.await.unwrap().expect("flush b");
        t_inc.await.unwrap().expect("concurrent increment");

        // Final flush drains any increment that landed after the first two flushes.
        meter_a.flush(sink.as_ref()).await.expect("final flush");

        let total_billed: u64 = sink
            .exported
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.quantity)
            .sum();
        assert_eq!(
            total_billed, 22,
            "total billed must equal exact recorded usage (10 + 5 + 7) with zero double-billing or lost increments"
        );
        assert_eq!(meter_a.get_usage("tenant-1", "api_calls").await.unwrap(), 0);

        // Verify rollback on export failure preserves counts for retry.
        meter_a
            .record_usage("tenant-1", "api_calls", 4)
            .await
            .expect("record 4");
        sink.fail_next.store(true, Ordering::SeqCst);
        assert!(meter_a.flush(sink.as_ref()).await.is_err());
        assert_eq!(meter_a.get_usage("tenant-1", "api_calls").await.unwrap(), 4);

        backend.close().await;
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }
}
