use super::*;

#[tokio::test]
async fn test_sqlite_test_fixture_creates_and_cleans_up() {
    let fixture = SqliteTestFixture::create().await.expect("fixture creates");
    let path = fixture.path().to_path_buf();
    assert!(path.exists(), "database file must exist on disk");

    fixture
        .pool()
        .backend()
        .execute("CREATE TABLE users (id INT, name TEXT)")
        .await
        .expect("execute");
    fixture
        .pool()
        .backend()
        .execute("INSERT INTO users VALUES (1, 'Alice')")
        .await
        .expect("insert");

    let count: Option<String> = fixture
        .pool()
        .backend()
        .query_scalar("SELECT COUNT(*) FROM users")
        .await
        .expect("count");
    assert_eq!(count.as_deref(), Some("1"));

    // Drop fixture
    drop(fixture);

    // Path must be removed from disk
    assert!(!path.exists(), "database file must be cleaned up on drop");
}

#[tokio::test]
async fn test_transaction_fixture_auto_rollback_on_drop() {
    let fixture = SqliteTestFixture::create().await.expect("fixture creates");
    fixture
        .pool()
        .backend()
        .execute("CREATE TABLE items (val INT)")
        .await
        .expect("create table");

    // Transaction 1: inserted but dropped without commit
    {
        let tx = fixture.begin_test_tx().await.expect("begin tx");
        tx.execute("INSERT INTO items VALUES (999)")
            .await
            .expect("insert in tx");
        // drop tx
    }

    let count: Option<String> = fixture
        .pool()
        .backend()
        .query_scalar("SELECT COUNT(*) FROM items")
        .await
        .expect("count");
    assert_eq!(
        count.as_deref(),
        Some("0"),
        "uncommitted transaction must be rolled back on drop"
    );

    // Transaction 2: committed
    {
        let tx = fixture.begin_test_tx().await.expect("begin tx");
        tx.execute("INSERT INTO items VALUES (123)")
            .await
            .expect("insert in tx");
        tx.commit().await.expect("commit");
    }

    let count2: Option<String> = fixture
        .pool()
        .backend()
        .query_scalar("SELECT COUNT(*) FROM items")
        .await
        .expect("count");
    assert_eq!(
        count2.as_deref(),
        Some("1"),
        "committed transaction must persist"
    );
    fixture.close().await.expect("fixture close");
}

#[tokio::test]
async fn test_sqlite_test_fixture_explicit_close_and_cleanup() {
    let fixture = SqliteTestFixture::create().await.expect("fixture creates");
    let path = fixture.path().to_path_buf();
    assert!(path.exists());

    fixture
        .pool()
        .backend()
        .execute("CREATE TABLE t (id INT)")
        .await
        .expect("execute");
    fixture
        .close()
        .await
        .expect("explicit close should succeed");

    assert!(!path.exists(), "database file must be cleaned up on close");
    assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
    assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
}

#[tokio::test]
async fn test_transaction_fixture_explicit_rollback() {
    let fixture = SqliteTestFixture::create().await.expect("fixture creates");
    fixture
        .pool()
        .backend()
        .execute("CREATE TABLE t (v INT)")
        .await
        .expect("create");

    let tx = fixture.begin_test_tx().await.expect("tx");
    tx.execute("INSERT INTO t VALUES (42)")
        .await
        .expect("insert");
    tx.rollback()
        .await
        .expect("explicit rollback should succeed");

    let count: Option<String> = fixture
        .pool()
        .backend()
        .query_scalar("SELECT COUNT(*) FROM t")
        .await
        .expect("count");
    assert_eq!(count.as_deref(), Some("0"));

    fixture.close().await.expect("close");
}

#[tokio::test]
async fn test_parallel_fixtures_deterministic_isolation() {
    let mut handles = Vec::new();
    for i in 0..5 {
        handles.push(tokio::spawn(async move {
            let fixture = SqliteTestFixture::create().await.expect("create");
            fixture
                .pool()
                .backend()
                .execute("CREATE TABLE p (v INT)")
                .await
                .expect("create");
            fixture
                .pool()
                .backend()
                .execute(&format!("INSERT INTO p VALUES ({i})"))
                .await
                .expect("insert");
            let count: Option<String> = fixture
                .pool()
                .backend()
                .query_scalar("SELECT COUNT(*) FROM p")
                .await
                .expect("count");
            assert_eq!(count.as_deref(), Some("1"));
            let path = fixture.path().to_path_buf();
            fixture.close().await.expect("close");
            assert!(!path.exists());
        }));
    }

    for h in handles {
        h.await.expect("task completes successfully");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_transaction_fixture_multi_thread_sync_rollback() {
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Debug)]
    struct GatedRollbackTx {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        rolled_back: Arc<AtomicBool>,
    }

    impl DatabaseTransaction for GatedRollbackTx {
        fn dialect(&self) -> &'static str {
            "postgres"
        }

        fn execute_params(
            &self,
            _sql: &str,
            _params: &[rume_data::persistence::DbValue],
        ) -> rume_data::persistence::BoxFuture<'_, Result<u64, PersistenceError>> {
            Box::pin(async { Ok(0) })
        }

        fn query_scalar_params(
            &self,
            _sql: &str,
            _params: &[rume_data::persistence::DbValue],
        ) -> rume_data::persistence::BoxFuture<'_, Result<Option<String>, PersistenceError>>
        {
            Box::pin(async { Ok(None) })
        }

        fn fetch_all(
            &self,
            _sql: &str,
            _params: &[rume_data::persistence::DbValue],
        ) -> rume_data::persistence::BoxFuture<
            '_,
            Result<Vec<rume_data::persistence::DbRow>, PersistenceError>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn fetch_optional(
            &self,
            _sql: &str,
            _params: &[rume_data::persistence::DbValue],
        ) -> rume_data::persistence::BoxFuture<
            '_,
            Result<Option<rume_data::persistence::DbRow>, PersistenceError>,
        > {
            Box::pin(async { Ok(None) })
        }

        fn commit(&self) -> rume_data::persistence::BoxFuture<'_, Result<(), PersistenceError>> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(&self) -> rume_data::persistence::BoxFuture<'_, Result<(), PersistenceError>> {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            let flag = Arc::clone(&self.rolled_back);
            Box::pin(async move {
                let release_notified = release.notified();
                tokio::pin!(release_notified);
                release_notified.as_mut().enable();
                entered.notify_waiters();
                release_notified.await;
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    // 1. Deterministic barrier proof (no timer/sleep): prove `drop` stays blocked while
    // `rollback()` is paused on `release`, and returns only after `release.notify_waiters()`.
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let rolled_back = Arc::new(AtomicBool::new(false));
    let drop_returned = Arc::new(AtomicBool::new(false));

    let entered_notified = entered.notified();
    tokio::pin!(entered_notified);
    entered_notified.as_mut().enable();

    let tx = TestTransactionFixture::new(GatedRollbackTx {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        rolled_back: Arc::clone(&rolled_back),
    });
    let drop_returned_clone = Arc::clone(&drop_returned);
    let drop_task = tokio::spawn(async move {
        drop(tx);
        drop_returned_clone.store(true, Ordering::SeqCst);
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        entered_notified.as_mut(),
    )
    .await
    .expect("rollback starts");
    assert!(
        !drop_returned.load(Ordering::SeqCst),
        "drop() must remain blocked until rollback() is released"
    );
    assert!(!rolled_back.load(Ordering::SeqCst));

    release.notify_waiters();
    drop_task.await.expect("drop task completes");
    assert!(
        rolled_back.load(Ordering::SeqCst) && drop_returned.load(Ordering::SeqCst),
        "multi_thread drop must synchronously complete rollback before returning from drop()"
    );

    // 2. Real SQLite data check.
    let fixture = SqliteTestFixture::create().await.expect("fixture creates");
    fixture
        .pool()
        .backend()
        .execute("CREATE TABLE mt_items (val INT)")
        .await
        .expect("create table");

    {
        let tx = fixture.begin_test_tx().await.expect("begin tx");
        tx.execute("INSERT INTO mt_items VALUES (777)")
            .await
            .expect("insert in tx");
    }

    let count: Option<String> = fixture
        .pool()
        .backend()
        .query_scalar("SELECT COUNT(*) FROM mt_items")
        .await
        .expect("count");
    assert_eq!(
        count.as_deref(),
        Some("0"),
        "uncommitted transaction must roll back synchronously on multi_thread drop"
    );

    fixture.close().await.expect("fixture close");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_fixture_close_times_out_when_tx_open_and_unlinks_on_drop() {
    let fixture = SqliteTestFixture::create().await.expect("fixture creates");
    let db_path = fixture.path().to_path_buf();
    assert!(
        db_path.exists(),
        "sqlite fixture file must exist before close"
    );

    // Hold a live transaction checked out from the SQLx pool while calling `close()`.
    let _live_tx = fixture.begin_test_tx().await.expect("begin tx");

    // Pause Tokio clock AFTER SQLite connection setup so the 5s close() timeout
    // fires deterministically without waiting 5s of wall-clock time.
    tokio::time::pause();

    let err = fixture
        .close()
        .await
        .expect_err("close() must time out when a transaction is still checked out");
    assert!(
        matches!(err, PersistenceError::ConnectionFailed(ref msg) if msg.contains("timed out draining sqlite pool")),
        "expected timeout error, got: {err:?}"
    );
    assert!(
        !db_path.exists(),
        "Drop on the failed close() path must still unlink the temporary sqlite file"
    );
}
