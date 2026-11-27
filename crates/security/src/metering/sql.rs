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
            const BATCH_CLAIM_LEASE_MS: i64 = 60_000;
            let dialect = self.backend.dialect();
            let claim_token = uuid::Uuid::new_v4().to_string();

            // Phase 1 (short DB transaction — ZERO network I/O):
            // Why: decoupling DB locks from `BillingSink` network I/O prevents a slow
            // billing RPC from holding SQLite's RESERVED write lock or Postgres/MySQL
            // `FOR UPDATE` row locks and blocking concurrent `record_usage()` calls (`RC-04`).
            // Atomically claim any previously unexported pending batches, snapshot & decrement
            // non-zero counters into a new claimed `rume_usage_export_batches` row, and commit.
            let tx = self
                .backend
                .begin_immediate_transaction()
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;

            let mut batches_to_export: Vec<(String, Vec<UsageRecord>)> = Vec::new();

            let pending_stmt = select_sql(
                dialect,
                sql::PG_USAGE_METERING_BATCH_LIST_PENDING,
                sql::MYSQL_USAGE_METERING_BATCH_LIST_PENDING,
                sql::SQLITE_USAGE_METERING_BATCH_LIST_PENDING,
            );
            let pending_rows = match tx.fetch_all(pending_stmt, &[]).await {
                Ok(rows) => rows,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(MeteringError::Store(e.to_string()));
                }
            };
            let claim_stmt = select_sql(
                dialect,
                sql::PG_USAGE_METERING_BATCH_CLAIM,
                sql::MYSQL_USAGE_METERING_BATCH_CLAIM,
                sql::SQLITE_USAGE_METERING_BATCH_CLAIM,
            );
            for row in &pending_rows {
                let batch_id: String = match row.get("batch_id") {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(MeteringError::Store(e.to_string()));
                    }
                };
                let payload_json: String = match row.get("payload_json") {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(MeteringError::Store(e.to_string()));
                    }
                };
                let records: Vec<UsageRecord> = match serde_json::from_str(&payload_json) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(MeteringError::Store(format!(
                            "corrupt usage export batch '{batch_id}': {e}"
                        )));
                    }
                };
                if let Err(e) = tx
                    .execute_params(
                        claim_stmt,
                        &[
                            DbValue::from(claim_token.as_str()),
                            DbValue::from(BATCH_CLAIM_LEASE_MS),
                            DbValue::from(batch_id.as_str()),
                        ],
                    )
                    .await
                {
                    let _ = tx.rollback().await;
                    return Err(MeteringError::Store(e.to_string()));
                }
                batches_to_export.push((batch_id, records));
            }

            let list_stmt = select_sql(
                dialect,
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

            if !rows.is_empty() {
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

                if !ids_and_records.is_empty() {
                    let dec_stmt = select_sql(
                        dialect,
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

                    let records: Vec<UsageRecord> =
                        ids_and_records.into_iter().map(|(_, _, r)| r).collect();
                    let payload_json = match serde_json::to_string(&records) {
                        Ok(j) => j,
                        Err(err) => {
                            let _ = tx.rollback().await;
                            return Err(MeteringError::Store(err.to_string()));
                        }
                    };
                    let batch_id = uuid::Uuid::new_v4().to_string();
                    let insert_batch_stmt = select_sql(
                        dialect,
                        sql::PG_USAGE_METERING_BATCH_INSERT,
                        sql::MYSQL_USAGE_METERING_BATCH_INSERT,
                        sql::SQLITE_USAGE_METERING_BATCH_INSERT,
                    );
                    if let Err(err) = tx
                        .execute_params(
                            insert_batch_stmt,
                            &[
                                DbValue::from(batch_id.as_str()),
                                DbValue::from(payload_json.as_str()),
                                DbValue::from(claim_token.as_str()),
                                DbValue::from(BATCH_CLAIM_LEASE_MS),
                            ],
                        )
                        .await
                    {
                        let _ = tx.rollback().await;
                        return Err(MeteringError::Store(err.to_string()));
                    }
                    batches_to_export.push((batch_id, records));
                }
            }

            tx.commit()
                .await
                .map_err(|e| MeteringError::Store(e.to_string()))?;

            if batches_to_export.is_empty() {
                return Ok(0);
            }

            // Phase 2 (OUTSIDE any DB transaction): export each claimed batch to `sink`.
            let mark_exported_stmt = select_sql(
                dialect,
                sql::PG_USAGE_METERING_BATCH_MARK_EXPORTED,
                sql::MYSQL_USAGE_METERING_BATCH_MARK_EXPORTED,
                sql::SQLITE_USAGE_METERING_BATCH_MARK_EXPORTED,
            );
            let release_claim_stmt = select_sql(
                dialect,
                sql::PG_USAGE_METERING_BATCH_RELEASE_CLAIM,
                sql::MYSQL_USAGE_METERING_BATCH_RELEASE_CLAIM,
                sql::SQLITE_USAGE_METERING_BATCH_RELEASE_CLAIM,
            );

            let mut exported_records = 0usize;
            for (idx, (batch_id, records)) in batches_to_export.iter().enumerate() {
                if let Err(err) = sink.export_batch(records).await {
                    // Release claims on the failed batch and any remaining claimed batches
                    // fenced by `claim_token` so a stalled worker whose lease expired can
                    // never clear another replica's active claim (`RC-04a`).
                    for (pending_id, _) in &batches_to_export[idx..] {
                        let _ = self
                            .backend
                            .execute_params(
                                release_claim_stmt,
                                &[
                                    DbValue::from(pending_id.as_str()),
                                    DbValue::from(claim_token.as_str()),
                                ],
                            )
                            .await;
                    }
                    return Err(err);
                }
                if let Err(e) = self
                    .backend
                    .execute_params(mark_exported_stmt, &[DbValue::from(batch_id.as_str())])
                    .await
                {
                    // Why: release claims on all remaining unexported batches so a DB error
                    // during `mark_exported` does not strand `batches_to_export[idx + 1..]`
                    // for the full 60s claim lease (`RC-38`).
                    for (pending_id, _) in &batches_to_export[idx + 1..] {
                        let _ = self
                            .backend
                            .execute_params(
                                release_claim_stmt,
                                &[
                                    DbValue::from(pending_id.as_str()),
                                    DbValue::from(claim_token.as_str()),
                                ],
                            )
                            .await;
                    }
                    return Err(MeteringError::Store(e.to_string()));
                }
                exported_records += records.len();
            }

            Ok(exported_records)
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

        // Verify failed export leaves the staged batch pending so the next flush retries
        // without losing usage or double-decrementing.
        meter_a
            .record_usage("tenant-1", "api_calls", 4)
            .await
            .expect("record 4");
        sink.fail_next.store(true, Ordering::SeqCst);
        assert!(meter_a.flush(sink.as_ref()).await.is_err());
        let retried = meter_a
            .flush(sink.as_ref())
            .await
            .expect("retry flush after sink recovery");
        assert_eq!(retried, 1);
        let total_after_retry: u64 = sink
            .exported
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.quantity)
            .sum();
        assert_eq!(total_after_retry, 26);
        assert_eq!(meter_a.flush(sink.as_ref()).await.unwrap(), 0);

        backend.close().await;
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[tokio::test]
    async fn slow_billing_sink_does_not_block_concurrent_record_usage() {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        use rume_data::persistence::{DatabaseConfig, SqliteBackend};
        use tokio::sync::Notify;

        struct SlowSignalingSink {
            started: Arc<Notify>,
            delay: Duration,
            exported: Mutex<Vec<UsageRecord>>,
        }

        impl BillingSink for SlowSignalingSink {
            fn export_batch<'a>(
                &'a self,
                records: &'a [UsageRecord],
            ) -> BoxFuture<'a, Result<(), MeteringError>> {
                let records = records.to_vec();
                let started = Arc::clone(&self.started);
                let delay = self.delay;
                Box::pin(async move {
                    started.notify_waiters();
                    tokio::time::sleep(delay).await;
                    self.exported.lock().unwrap().extend(records);
                    Ok(())
                })
            }
        }

        let db_path = std::env::temp_dir().join(format!(
            "rume-metering-rc04-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let cfg = DatabaseConfig::new(db_path.to_string_lossy().as_ref()).with_max_connections(4);
        let backend = Arc::new(SqliteBackend::new(&cfg).await.expect("open sqlite"));

        let meter = SqlUsageMeter::new(Arc::clone(&backend));
        meter.ensure_schema().await.expect("ensure_schema");
        meter.record_usage("t1", "api", 5).await.expect("seed 5");

        let started = Arc::new(Notify::new());
        let sink = Arc::new(SlowSignalingSink {
            started: Arc::clone(&started),
            delay: Duration::from_millis(300),
            exported: Mutex::new(Vec::new()),
        });

        let flush_meter = meter.clone();
        let flush_sink = Arc::clone(&sink);
        let flush_task = tokio::spawn(async move { flush_meter.flush(flush_sink.as_ref()).await });

        // Wait until `flush` is actively awaiting the 300ms BillingSink RPC.
        started.notified().await;

        let t0 = Instant::now();
        meter
            .record_usage("t1", "api", 1)
            .await
            .expect("record_usage during slow flush");
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "record_usage must not block on 300ms BillingSink RPC (took {elapsed:?})"
        );

        flush_task.await.unwrap().expect("slow flush completes");
        // Final flush exports the concurrent +1.
        meter.flush(sink.as_ref()).await.expect("drain +1");
        let total: u64 = sink
            .exported
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.quantity)
            .sum();
        assert_eq!(total, 6);

        backend.close().await;
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[tokio::test]
    async fn stale_claim_token_cannot_release_reclaimed_usage_batch() {
        use rume_data::persistence::{DatabaseConfig, SqliteBackend};

        let backend = Arc::new(
            SqliteBackend::new(&DatabaseConfig::new(":memory:"))
                .await
                .expect("open sqlite memory"),
        );
        let meter = SqlUsageMeter::new(Arc::clone(&backend));
        meter.ensure_schema().await.expect("ensure_schema");

        // Worker A inserts a staged batch with `tok-a`.
        backend
            .execute_params(
                sql::SQLITE_USAGE_METERING_BATCH_INSERT,
                &[
                    DbValue::from("batch-1"),
                    DbValue::from("[]"),
                    DbValue::from("tok-a"),
                    DbValue::from(60_000_i64),
                ],
            )
            .await
            .expect("insert batch");

        // Worker B reclaims the batch with `tok-b`.
        backend
            .execute_params(
                sql::SQLITE_USAGE_METERING_BATCH_CLAIM,
                &[
                    DbValue::from("tok-b"),
                    DbValue::from(60_000_i64),
                    DbValue::from("batch-1"),
                ],
            )
            .await
            .expect("reclaim batch");

        // Stale Worker A attempts to release with `tok-a` -> 0 rows affected, claim stays owned by `tok-b`.
        let affected_stale = backend
            .execute_params(
                sql::SQLITE_USAGE_METERING_BATCH_RELEASE_CLAIM,
                &[DbValue::from("batch-1"), DbValue::from("tok-a")],
            )
            .await
            .expect("stale release");
        assert_eq!(
            affected_stale, 0,
            "stale claim_token must not clear another worker's active claim"
        );

        // Active Worker B releases with `tok-b` -> 1 row affected.
        let affected_active = backend
            .execute_params(
                sql::SQLITE_USAGE_METERING_BATCH_RELEASE_CLAIM,
                &[DbValue::from("batch-1"), DbValue::from("tok-b")],
            )
            .await
            .expect("active release");
        assert_eq!(affected_active, 1);
    }
}
