use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;
use super::preflight::{
    MYSQL_CORE_DOWN, MYSQL_MIGRATION_DELETE, MYSQL_MIGRATION_FIND_CHECKSUM,
    MYSQL_MIGRATION_FIND_ID, MYSQL_MIGRATION_INSERT, MYSQL_CLOCK, NEXT_APPLIED_ORDER_SQL,
    PG_CORE_DOWN, select_rollback_target, SQLITE_CORE_DOWN, SQLITE_MIGRATION_FIND_CHECKSUM,
    SQLITE_MIGRATION_INSERT,
};
use crate::persistence::backend::{BoxFuture, DatabaseBackend, DatabaseTransaction, DbValue};
use crate::persistence::error::PersistenceError;
use crate::persistence::row::DbRow;

#[derive(Debug, Clone)]
struct MockMysqlTx;

impl DatabaseTransaction for MockMysqlTx {
    fn dialect(&self) -> &'static str {
        "mysql"
    }

    fn execute_params(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
    }

    fn query_scalar_params(
        &self,
        _query: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
    }

    fn fetch_all(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
    }

    fn fetch_optional(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        Box::pin(async { Err(PersistenceError::TransactionError("unexpected tx".into())) })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async {
            Err(PersistenceError::TransactionError(
                "unexpected commit".into(),
            ))
        })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async {
            Err(PersistenceError::TransactionError(
                "unexpected rollback".into(),
            ))
        })
    }
}

#[derive(Debug, Clone, Default)]
struct MockMysqlBackend {
    executed_sql: Arc<Mutex<Vec<String>>>,
    ledger: Arc<Mutex<BTreeMap<String, String>>>,
    begin_tx_calls: Arc<Mutex<usize>>,
}

impl DatabaseBackend for MockMysqlBackend {
    type Transaction = MockMysqlTx;

    fn dialect(&self) -> &'static str {
        "mysql"
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let statement = statement.to_string();
        let params = params.to_vec();
        let executed_sql = Arc::clone(&self.executed_sql);
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            executed_sql.lock().unwrap().push(statement.clone());
            if statement.contains("FAIL_MIGRATION") {
                return Err(PersistenceError::QueryFailed("simulated failure".into()));
            }
            if statement == MYSQL_MIGRATION_INSERT {
                if let (Some(DbValue::Text(id)), Some(DbValue::Text(checksum))) =
                    (params.first(), params.get(3))
                {
                    ledger.lock().unwrap().insert(id.clone(), checksum.clone());
                }
            } else if statement == MYSQL_MIGRATION_DELETE
                && let Some(DbValue::Text(id)) = params.first()
            {
                ledger.lock().unwrap().remove(id);
            }
            Ok(1)
        })
    }

    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let query = query.to_string();
        let params = params.to_vec();
        let executed_sql = Arc::clone(&self.executed_sql);
        let ledger = Arc::clone(&self.ledger);
        Box::pin(async move {
            executed_sql.lock().unwrap().push(query.clone());
            if query == MYSQL_MIGRATION_FIND_CHECKSUM {
                if let Some(DbValue::Text(id)) = params.first() {
                    return Ok(ledger.lock().unwrap().get(id).cloned());
                }
            } else if query == MYSQL_MIGRATION_FIND_ID {
                if let Some(DbValue::Text(id)) = params.first() {
                    return Ok(ledger.lock().unwrap().contains_key(id).then(|| id.clone()));
                }
            } else if query == MYSQL_CLOCK {
                return Ok(Some("1700000000".to_string()));
            }
            Ok(None)
        })
    }

    fn fetch_all(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_optional(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        Box::pin(async { Ok(None) })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        let calls = Arc::clone(&self.begin_tx_calls);
        Box::pin(async move {
            *calls.lock().unwrap() += 1;
            Err(PersistenceError::TransactionError(
                "MySQL migrations must not call begin_transaction()".into(),
            ))
        })
    }
}

#[test]
fn core_down_scripts_never_drop_ledger_or_audit_tables() {
    for (dialect, sql) in [
        ("postgres", PG_CORE_DOWN),
        ("mysql", MYSQL_CORE_DOWN),
        ("sqlite", SQLITE_CORE_DOWN),
    ] {
        assert!(
            !sql.contains("rume_migrations"),
            "{dialect} core_down.sql must not drop rume_migrations"
        );
        assert!(
            !sql.contains("rume_audit_log"),
            "{dialect} core_down.sql must not drop rume_audit_log"
        );
    }
}

#[test]
fn with_rume_core_registers_core_and_audit_ahead_of_app_migrations() {
    for dialect in ["postgres", "mysql", "sqlite"] {
        let migrator = Migrator::new()
            .add_migration(Migration::from_sql(
                "20261105_001_app",
                "App migration",
                "CREATE TABLE app_items (id INT);",
            ))
            .with_rume_core(dialect);

        let migrations = migrator.migrations();
        assert_eq!(
            migrations.len(),
            3,
            "{dialect} must register 2 core migrations before app migrations"
        );
        assert_eq!(migrations[0].id, RUME_CORE_MIGRATION_ID);
        assert_eq!(migrations[1].id, RUME_AUDIT_MIGRATION_ID);
        assert!(
            migrations[1].down_sql.is_none(),
            "{dialect} audit migration must be irreversible to protect pre-existing audit history"
        );
        assert_eq!(migrations[2].id, "20261105_001_app");
    }

    let unknown = Migrator::new().with_rume_core("unknown_dialect");
    assert!(unknown.migrations().is_empty());
}

#[tokio::test]
async fn mysql_dialect_without_migration_lock_capability_fails_closed_before_mutation() {
    let backend = MockMysqlBackend::default();
    let migrator = Migrator::new().add_migration(
        Migration::from_sql("001_first", "first", "CREATE TABLE t1 (id INT);")
            .with_down("DROP TABLE t1;"),
    );

    let run_err = migrator
        .run_pending_backend(&backend)
        .await
        .expect_err("MySQL backend without migration lock capability must fail closed");
    assert!(matches!(run_err, PersistenceError::TransactionError(_)));
    assert!(
        backend.executed_sql.lock().unwrap().is_empty(),
        "no SQL must execute before acquiring MySQL migration lock"
    );
    assert!(backend.ledger.lock().unwrap().is_empty());

    let rb_err = migrator
        .rollback_last_backend(&backend)
        .await
        .expect_err("MySQL rollback without migration lock capability must fail closed");
    assert!(matches!(rb_err, PersistenceError::TransactionError(_)));
    assert!(
        backend.executed_sql.lock().unwrap().is_empty(),
        "no SQL must execute during unlocked MySQL rollback"
    );
}

#[test]
fn select_rollback_target_prefers_actual_application_order_over_registry_order() {
    let migrations = vec![
        Migration::from_sql("core", "core", "SELECT 1;"),
        Migration::from_sql("audit", "audit", "SELECT 1;"),
        Migration::from_sql("app_a", "app_a", "SELECT 1;"),
    ];

    // Upgraded DB: core (order 1) and app_a (order 2) applied first, then audit (order 3)
    // applied in the same second. Even though `audit` precedes `app_a` in `migrations`,
    // actual application order selects `audit`.
    let applied = vec![
        ("audit".to_string(), 3, 1700000000),
        ("app_a".to_string(), 2, 1700000000),
        ("core".to_string(), 1, 1700000000),
    ];
    let target = select_rollback_target(&migrations, &applied).expect("target present");
    assert_eq!(target.id, "audit");
}

#[derive(Debug, Clone, Default)]
struct MockGenericTx {
    executed_sql: Arc<Mutex<Vec<String>>>,
    ledger: Arc<Mutex<BTreeMap<String, String>>>,
    has_applied_order: Arc<Mutex<bool>>,
}

impl DatabaseTransaction for MockGenericTx {
    fn dialect(&self) -> &'static str {
        "custom_sql"
    }

    fn execute_params(
        &self,
        statement: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let statement = statement.to_string();
        let params = params.to_vec();
        let executed_sql = Arc::clone(&self.executed_sql);
        let ledger = Arc::clone(&self.ledger);
        let has_applied_order = Arc::clone(&self.has_applied_order);
        Box::pin(async move {
            executed_sql.lock().unwrap().push(statement.clone());
            if statement == SQLITE_MIGRATION_INSERT {
                if !*has_applied_order.lock().unwrap() {
                    return Err(PersistenceError::QueryFailed(
                        "column applied_order does not exist".into(),
                    ));
                }
                if let (Some(DbValue::Text(id)), Some(DbValue::Text(checksum))) =
                    (params.first(), params.get(3))
                {
                    ledger.lock().unwrap().insert(id.clone(), checksum.clone());
                }
            }
            Ok(1)
        })
    }

    fn query_scalar_params(
        &self,
        query: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let query = query.to_string();
        let executed_sql = Arc::clone(&self.executed_sql);
        let has_applied_order = Arc::clone(&self.has_applied_order);
        Box::pin(async move {
            executed_sql.lock().unwrap().push(query.clone());
            if query.contains("applied_order") && !*has_applied_order.lock().unwrap() {
                return Err(PersistenceError::QueryFailed(
                    "column applied_order does not exist".into(),
                ));
            }
            Ok(Some("1".to_string()))
        })
    }

    fn fetch_all(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_optional(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        Box::pin(async { Ok(None) })
    }

    fn commit(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    fn rollback(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, Default)]
struct MockGenericBackend {
    executed_sql: Arc<Mutex<Vec<String>>>,
    ledger: Arc<Mutex<BTreeMap<String, String>>>,
    has_applied_order: Arc<Mutex<bool>>,
}

impl DatabaseBackend for MockGenericBackend {
    type Transaction = MockGenericTx;

    fn dialect(&self) -> &'static str {
        "custom_sql"
    }

    fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
        Box::pin(async { Ok(()) })
    }

    fn execute_params(
        &self,
        statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<u64, PersistenceError>> {
        let statement = statement.to_string();
        let executed_sql = Arc::clone(&self.executed_sql);
        let has_applied_order = Arc::clone(&self.has_applied_order);
        Box::pin(async move {
            executed_sql.lock().unwrap().push(statement.clone());
            if statement.starts_with("ALTER TABLE rume_migrations ADD COLUMN")
                && statement.contains("applied_order")
            {
                let mut col = has_applied_order.lock().unwrap();
                if *col {
                    return Err(PersistenceError::QueryFailed(
                        "duplicate column applied_order".into(),
                    ));
                }
                *col = true;
            }
            Ok(1)
        })
    }

    fn query_scalar_params(
        &self,
        query: &str,
        params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<String>, PersistenceError>> {
        let query = query.to_string();
        let params = params.to_vec();
        let executed_sql = Arc::clone(&self.executed_sql);
        let ledger = Arc::clone(&self.ledger);
        let has_applied_order = Arc::clone(&self.has_applied_order);
        Box::pin(async move {
            executed_sql.lock().unwrap().push(query.clone());
            if query.contains("applied_order") && !*has_applied_order.lock().unwrap() {
                return Err(PersistenceError::QueryFailed(
                    "column applied_order does not exist".into(),
                ));
            }
            if query == SQLITE_MIGRATION_FIND_CHECKSUM
                && let Some(DbValue::Text(id)) = params.first()
            {
                return Ok(ledger.lock().unwrap().get(id).cloned());
            }
            Ok(None)
        })
    }

    fn fetch_all(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Vec<DbRow>, PersistenceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_optional(
        &self,
        _statement: &str,
        _params: &[DbValue],
    ) -> BoxFuture<'_, Result<Option<DbRow>, PersistenceError>> {
        Box::pin(async { Ok(None) })
    }

    fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
        let tx = MockGenericTx {
            executed_sql: Arc::clone(&self.executed_sql),
            ledger: Arc::clone(&self.ledger),
            has_applied_order: Arc::clone(&self.has_applied_order),
        };
        Box::pin(async move { Ok(tx) })
    }
}

#[tokio::test]
async fn generic_backend_fallback_ensures_applied_order_column_before_insert() {
    // Start with a legacy ledger table lacking `applied_order` (`has_applied_order = false`).
    let backend = MockGenericBackend::default();
    let migrator = Migrator::new().add_migration(Migration::from_sql(
        "001_custom",
        "custom migration",
        "CREATE TABLE t_custom (id INT);",
    ));

    let summary = migrator
        .run_pending_backend(&backend)
        .await
        .expect("generic fallback migration upgrades legacy table and succeeds");
    assert_eq!(summary.applied.len(), 1);
    assert!(
        *backend.has_applied_order.lock().unwrap(),
        "generic fallback must add applied_order column when missing"
    );

    let log = backend.executed_sql.lock().unwrap().clone();
    let alter_idx = log
        .iter()
        .position(|s| {
            s.starts_with("ALTER TABLE rume_migrations ADD COLUMN")
                && s.contains("applied_order")
        })
        .expect("ALTER TABLE ... ADD COLUMN applied_order must run on legacy ledger");
    let max_order_idx = log
        .iter()
        .position(|s| s == NEXT_APPLIED_ORDER_SQL)
        .expect("next_applied_order_tx must run");
    let insert_idx = log
        .iter()
        .position(|s| s == SQLITE_MIGRATION_INSERT)
        .expect("migration insert must run");
    assert!(
        alter_idx < max_order_idx && max_order_idx < insert_idx,
        "column setup must precede order-dependent queries and inserts: {log:?}"
    );

    // Second run when `applied_order` already exists must succeed without re-running ALTER.
    let count_alter = |entries: &[String]| {
        entries
            .iter()
            .filter(|s| {
                s.starts_with("ALTER TABLE rume_migrations ADD COLUMN")
                    && s.contains("applied_order")
            })
            .count()
    };
    let alter_count_before = count_alter(&log);
    let summary2 = migrator
        .run_pending_backend(&backend)
        .await
        .expect("second run with existing applied_order column succeeds");
    assert!(summary2.applied.is_empty());
    let log_after = backend.executed_sql.lock().unwrap().clone();
    assert_eq!(
        count_alter(&log_after),
        alter_count_before,
        "second run must skip ALTER TABLE when applied_order already exists"
    );
}
