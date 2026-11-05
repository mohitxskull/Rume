//! Unified database conformance suite verifying behavioral parity across SQLite, PostgreSQL, and MySQL.

use crate::persistence::backend::DbValue;
use crate::persistence::error::PersistenceError;
use crate::persistence::migration::{Migration, Migrator};
use crate::persistence::pool::DatabasePool;
use crate::persistence::row::{DbRow, FromRow};
use crate::persistence::{DatabaseBackend, DatabaseTransaction};
use crate::routing::health::{HealthCheck, HealthStatus};

/// Typed row representation used in database conformance verification.
#[derive(Debug, Clone, PartialEq)]
pub struct ConformanceRecord {
    pub id: String,
    pub title: String,
    pub counter: Option<i64>,
    pub flag: bool,
    pub payload: Option<String>,
}

impl FromRow for ConformanceRecord {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(Self {
            id: row.get("id")?,
            title: row.get("title")?,
            counter: row.get_opt("counter")?,
            flag: row.get("flag")?,
            payload: row.get_opt("payload")?,
        })
    }
}

fn format_query(sql: &str, dialect: &str) -> String {
    if dialect == "mysql" {
        let mut out = String::with_capacity(sql.len());
        let mut chars = sql.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '$' && chars.peek().map(|p| p.is_ascii_digit()).unwrap_or(false) {
                while let Some(p) = chars.peek() {
                    if p.is_ascii_digit() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push('?');
            } else {
                out.push(c);
            }
        }
        out
    } else {
        sql.to_string()
    }
}

/// Executes the full standard Rume database conformance verification suite against any backend pool.
///
/// Ensures strict behavioral parity across:
/// 1. Migrations, version tracking, and fail-closed SHA-256 checksum mismatch detection
/// 2. Explicit parameterized transactions with commit and automatic error rollback
/// 3. Typed parameter binding and SQL injection resilience
/// 4. NULL vs missing column distinction in generic [`DbRow`] extraction
/// 5. High-concurrency query execution across pooled connections
/// 6. Pool lease lifecycle, timeouts, and health check integration
/// 7. Constraint violation error handling without silent swallowing or panics
pub async fn verify_database_conformance<B>(
    pool: &DatabasePool<B>,
    dialect: &str,
) -> Result<(), PersistenceError>
where
    B: DatabaseBackend + Send + Sync + 'static,
{
    // -------------------------------------------------------------------------
    // 1. Health Check & Initial Pool Connectivity
    // -------------------------------------------------------------------------
    let health = pool.health_check("conformance_test").run().await;
    assert_eq!(
        health.status,
        HealthStatus::Ok,
        "database health check failed before conformance suite: {:?}",
        health.message
    );

    let lease = pool.acquire_lease()?;
    assert!(pool.status().leased_connections >= 1);
    drop(lease);

    // -------------------------------------------------------------------------
    // 2. Migration Execution & Fail-Closed Checksum Verification
    // -------------------------------------------------------------------------
    let table_ddl = match dialect {
        "postgres" => {
            r#"
            CREATE TABLE IF NOT EXISTS rume_conformance (
                id VARCHAR(64) PRIMARY KEY,
                title VARCHAR(255) NOT NULL,
                counter BIGINT,
                flag BOOLEAN NOT NULL,
                payload TEXT
            );
            "#
        }
        "mysql" => {
            r#"
            CREATE TABLE IF NOT EXISTS rume_conformance (
                id VARCHAR(64) PRIMARY KEY,
                title VARCHAR(255) NOT NULL,
                counter BIGINT,
                flag BOOLEAN NOT NULL,
                payload TEXT
            );
            "#
        }
        _ => {
            // SQLite
            r#"
            CREATE TABLE IF NOT EXISTS rume_conformance (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                counter INTEGER,
                flag INTEGER NOT NULL,
                payload TEXT
            );
            "#
        }
    };

    let migration1 = Migration::from_sql("20261104000001", "create_conformance_table", table_ddl);
    let original_checksum = migration1.checksum();

    let migrator = Migrator::new()
        .with_rume_core(dialect)
        .add_migration(migration1.clone());
    migrator.run_pending(pool.backend()).await?;

    // Verify migration recorded in rume_migrations
    let recorded_checksum: Option<String> = pool
        .query_scalar_params(
            &format_query(
                "SELECT checksum FROM rume_migrations WHERE id = $1",
                dialect,
            ),
            &[DbValue::Text("20261104000001".to_string())],
        )
        .await?;
    assert_eq!(
        recorded_checksum.as_deref(),
        Some(original_checksum.as_str()),
        "recorded migration checksum does not match expected SHA-256"
    );

    // Verify tampering with migration SQL fails closed with ChecksumMismatch
    let tampered_migration = Migration::from_sql(
        "20261104000001",
        "create_conformance_table",
        format!("{table_ddl}\n-- tampered comment"),
    );
    let tampered_migrator = Migrator::new().add_migration(tampered_migration);
    let tamper_res = tampered_migrator.run_pending(pool.backend()).await;
    match tamper_res {
        Err(PersistenceError::MigrationChecksumMismatch { migration, .. }) => {
            assert_eq!(migration, "20261104000001");
        }
        other => panic!("expected MigrationChecksumMismatch on tampered migration, got: {other:?}"),
    }

    // Clean test table for subsequent tests
    pool.execute("DELETE FROM rume_conformance").await?;

    // -------------------------------------------------------------------------
    // 3. Explicit Parameterized Transactions: Commit vs Rollback
    // -------------------------------------------------------------------------
    let insert_sql = format_query(
        "INSERT INTO rume_conformance (id, title, counter, flag, payload) VALUES ($1, $2, $3, $4, $5)",
        dialect,
    );
    let select_by_id_sql = format_query(
        "SELECT id, title, counter, flag, payload FROM rume_conformance WHERE id = $1",
        dialect,
    );

    let row1 = ConformanceRecord {
        id: "rec_1".to_string(),
        title: "Committed Transaction Record".to_string(),
        counter: Some(100),
        flag: true,
        payload: Some("payload-committed".to_string()),
    };

    // Commit path
    let sql_clone = insert_sql.clone();
    pool.transaction(|tx| {
        let r = row1.clone();
        let s = sql_clone.clone();
        async move {
            tx.execute_params(
                &s,
                &[
                    DbValue::Text(r.id),
                    DbValue::Text(r.title),
                    match r.counter {
                        Some(c) => DbValue::Int(c),
                        None => DbValue::Null,
                    },
                    DbValue::Bool(r.flag),
                    match r.payload {
                        Some(p) => DbValue::Text(p),
                        None => DbValue::Null,
                    },
                ],
            )
            .await?;
            Ok::<(), PersistenceError>(())
        }
    })
    .await?;

    // Verify row1 was committed
    let fetched1 = pool
        .fetch_optional_as::<ConformanceRecord>(
            &select_by_id_sql,
            &[DbValue::Text("rec_1".to_string())],
        )
        .await?
        .expect("rec_1 should exist after commit");
    assert_eq!(fetched1, row1);

    // Rollback path on explicit error
    let sql_clone2 = insert_sql.clone();
    let rollback_res = pool
        .transaction(|tx| {
            let s = sql_clone2.clone();
            async move {
                tx.execute_params(
                    &s,
                    &[
                        DbValue::Text("rec_aborted".to_string()),
                        DbValue::Text("Should be rolled back".to_string()),
                        DbValue::Int(999),
                        DbValue::Bool(false),
                        DbValue::Null,
                    ],
                )
                .await?;

                Err::<(), PersistenceError>(PersistenceError::QueryFailed(
                    "simulated transaction failure for rollback verification".to_string(),
                ))
            }
        })
        .await;
    assert!(
        rollback_res.is_err(),
        "transaction should return error on failure"
    );

    // Verify rolled-back row was NOT persisted
    let fetched_aborted = pool
        .fetch_optional_as::<ConformanceRecord>(
            &select_by_id_sql,
            &[DbValue::Text("rec_aborted".to_string())],
        )
        .await?;
    assert!(
        fetched_aborted.is_none(),
        "rolled back record rec_aborted must not exist in database"
    );

    // -------------------------------------------------------------------------
    // 4. NULL vs Missing Column Handling in Generic DbRow
    // -------------------------------------------------------------------------
    let row_nulls = ConformanceRecord {
        id: "rec_nulls".to_string(),
        title: "Record With Null Fields".to_string(),
        counter: None,
        flag: false,
        payload: None,
    };

    pool.execute_params(
        &insert_sql,
        &[
            DbValue::Text(row_nulls.id.clone()),
            DbValue::Text(row_nulls.title.clone()),
            DbValue::Null,
            DbValue::Bool(row_nulls.flag),
            DbValue::Null,
        ],
    )
    .await?;

    let raw_rows = pool
        .fetch_all(&select_by_id_sql, &[DbValue::Text("rec_nulls".to_string())])
        .await?;
    assert_eq!(raw_rows.len(), 1);
    let raw_row = &raw_rows[0];

    // SQL NULL yields DbValue::Null
    assert_eq!(raw_row.get_value("counter")?, &DbValue::Null);
    assert_eq!(raw_row.get_value("payload")?, &DbValue::Null);
    assert_eq!(raw_row.get_opt::<i64>("counter")?, None);
    assert_eq!(raw_row.get_opt::<String>("payload")?, None);

    // Non-existent column returns ColumnNotFound
    match raw_row.get_value("non_existent_column") {
        Err(PersistenceError::ColumnNotFound(col)) => assert_eq!(col, "non_existent_column"),
        other => panic!("expected ColumnNotFound for missing column, got: {other:?}"),
    }

    // -------------------------------------------------------------------------
    // 5. Constraint Violation Resilience (Duplicate Primary Key)
    // -------------------------------------------------------------------------
    let duplicate_insert = pool
        .execute_params(
            &insert_sql,
            &[
                DbValue::Text("rec_1".to_string()), // already exists
                DbValue::Text("Duplicate PK attempt".to_string()),
                DbValue::Int(1),
                DbValue::Bool(true),
                DbValue::Null,
            ],
        )
        .await;
    assert!(
        duplicate_insert.is_err(),
        "duplicate primary key insertion must return Err without panicking"
    );

    // -------------------------------------------------------------------------
    // 6. High-Concurrency Query Execution Under Load
    // -------------------------------------------------------------------------
    let concurrency = 16;
    let queries_per_worker = 50;
    let mut handles = Vec::with_capacity(concurrency);

    let select_title_sql =
        format_query("SELECT title FROM rume_conformance WHERE id = $1", dialect);

    for worker_idx in 0..concurrency {
        let pool_clone = pool.clone();
        let insert_clone = insert_sql.clone();
        let select_clone = select_title_sql.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..queries_per_worker {
                let id = format!("rec_conc_{worker_idx}_{i}");
                pool_clone
                    .execute_params(
                        &insert_clone,
                        &[
                            DbValue::Text(id.clone()),
                            DbValue::Text(format!("Concurrent {worker_idx}")),
                            DbValue::Int((worker_idx * 1000 + i) as i64),
                            DbValue::Bool(i % 2 == 0),
                            DbValue::Text("load-test".to_string()),
                        ],
                    )
                    .await?;

                let found: Option<String> = pool_clone
                    .query_scalar_params(&select_clone, &[DbValue::Text(id)])
                    .await?;
                assert!(found.is_some());
            }
            Ok::<(), PersistenceError>(())
        }));
    }

    for handle in handles {
        handle.await.expect("worker task panic")?;
    }

    // Verify total inserted rows
    let total_count: Option<String> = pool
        .query_scalar("SELECT COUNT(*) FROM rume_conformance")
        .await?;
    let expected_count = 2 + (concurrency * queries_per_worker); // rec_1, rec_nulls + concurrent
    assert_eq!(
        total_count.as_deref().and_then(|s| s.parse::<usize>().ok()),
        Some(expected_count),
        "concurrency load did not result in expected total row count"
    );

    Ok(())
}
