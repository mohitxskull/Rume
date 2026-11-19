//! Isolated database and transaction fixtures for test isolation.

#[cfg(feature = "sqlite")]
use rume_data::persistence::DatabaseConfig;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
use rume_data::persistence::{DatabaseBackend, DatabasePool};
use rume_data::persistence::{DatabaseTransaction, PersistenceError};
#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
#[cfg(any(feature = "postgres", all(test, feature = "sqlite")))]
use std::sync::Arc;

/// An RAII transaction guard that automatically rolls back the transaction on Drop
/// unless explicitly committed.
///
/// For SQLite transactions (or any driver dropped on a `#[tokio::test(flavor = "multi_thread")]`
/// runtime), `Drop` completes rollback synchronously (5s timeout). Synchronous drop rollback
/// barriers for Postgres/MySQL transactions require `#[tokio::test(flavor = "multi_thread")]`
/// or calling `.rollback().await` / `.close().await` explicitly.
pub struct TestTransactionFixture<T: DatabaseTransaction + 'static> {
    tx: Option<T>,
    committed: bool,
    #[cfg(feature = "postgres")]
    pending_rollbacks: Option<Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>>,
}

impl<T: DatabaseTransaction + 'static> TestTransactionFixture<T> {
    /// Creates a new fixture wrapping an active database transaction.
    pub fn new(tx: T) -> Self {
        Self {
            tx: Some(tx),
            committed: false,
            #[cfg(feature = "postgres")]
            pending_rollbacks: None,
        }
    }

    #[cfg(feature = "postgres")]
    fn with_pending_rollbacks(
        tx: T,
        pending_rollbacks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    ) -> Self {
        Self {
            tx: Some(tx),
            committed: false,
            pending_rollbacks: Some(pending_rollbacks),
        }
    }

    /// Returns a reference to the active transaction.
    pub fn tx(&self) -> &T {
        self.tx.as_ref().expect("transaction active")
    }

    /// Atomically commits the transaction, consuming the fixture and preventing rollback on Drop.
    pub async fn commit(mut self) -> Result<(), PersistenceError> {
        if let Some(tx) = self.tx.take() {
            self.committed = true;
            tx.commit().await?;
        }
        Ok(())
    }

    /// Discards and rolls back the transaction explicitly, awaiting completion.
    pub async fn rollback(mut self) -> Result<(), PersistenceError> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
        }
        Ok(())
    }

    /// Discards and rolls back the transaction explicitly, awaiting completion.
    pub async fn close(self) -> Result<(), PersistenceError> {
        self.rollback().await
    }
}

impl<T: DatabaseTransaction + 'static> std::ops::Deref for TestTransactionFixture<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.tx()
    }
}

impl<T: DatabaseTransaction + 'static> std::fmt::Debug for TestTransactionFixture<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestTransactionFixture")
            .field("committed", &self.committed)
            .finish()
    }
}

impl<T: DatabaseTransaction + 'static> Drop for TestTransactionFixture<T> {
    fn drop(&mut self) {
        if !self.committed
            && let Some(tx) = self.tx.take()
        {
            let timeout = std::time::Duration::from_secs(5);
            match tokio::runtime::Handle::try_current() {
                Ok(handle)
                    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
                {
                    // Why: Multi-thread Tokio supports `block_in_place` so other worker threads
                    // keep polling the I/O reactor while this thread waits synchronously for
                    // rollback (up to 5s).
                    tokio::task::block_in_place(|| {
                        let _ = handle.block_on(tokio::time::timeout(timeout, tx.rollback()));
                    });
                }
                _ if tx.dialect() == "sqlite" => {
                    // Why: `sqlx-sqlite` runs queries over channels on its own dedicated OS worker
                    // thread rather than Tokio's socket reactor, so awaiting `tx.rollback()` on a
                    // joined helper thread's runtime finishes synchronously without deadlocking
                    // `epoll_wait` on `current_thread` tests or outside Tokio.
                    let _ = std::thread::spawn(move || {
                        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            rt.block_on(async move {
                                let _ = tokio::time::timeout(timeout, tx.rollback()).await;
                            });
                        }
                    })
                    .join();
                }
                Ok(handle) => {
                    // Why: Outer-reactor TCP sockets (`postgres`, `mysql`) on a `current_thread`
                    // runtime cannot be driven by a joined helper thread because blocking the sole
                    // test thread halts `epoll_wait` and deadlocks. Spawn the rollback on `handle`
                    // and record its `AbortHandle` on any owning `PostgresSchemaFixture` so that if
                    // the fixture is dropped immediately (`drop(tx); drop(fixture);` without an
                    // intervening `.await`), `PostgresSchemaFixture::drop` aborts the unpolled task
                    // and terminates the backend session on the server before synchronously
                    // executing `DROP SCHEMA ... CASCADE`.
                    let jh = handle.spawn(async move {
                        let _ = tx.rollback().await;
                    });
                    #[cfg(feature = "postgres")]
                    if let Some(ref pending) = self.pending_rollbacks
                        && let Ok(mut list) = pending.lock()
                    {
                        list.push(jh.abort_handle());
                    }
                    #[cfg(not(feature = "postgres"))]
                    drop(jh);
                }
                Err(_) => {
                    // Outside any Tokio runtime, TCP sockets have no active reactor to drive
                    // `rollback().await`; dropping `tx` falls back to SQLx's connection cleanup.
                }
            }
        }
    }
}

/// An isolated SQLite database backed by a temporary file on disk, automatically deleted on Drop.
#[cfg(feature = "sqlite")]
pub struct SqliteTestFixture {
    path: PathBuf,
    pool: DatabasePool<rume_data::persistence::SqliteBackend>,
    closed: bool,
}

#[cfg(feature = "sqlite")]
impl SqliteTestFixture {
    /// Creates a unique temporary SQLite database file and initializes a connection pool.
    pub async fn create() -> Result<Self, PersistenceError> {
        let temp_dir = std::env::temp_dir();
        let db_name = format!("rume_test_{}.db", uuid::Uuid::new_v4().simple());
        let path = temp_dir.join(db_name);
        let path_str = path.to_string_lossy().to_string();

        let backend = rume_data::persistence::SqliteBackend::open(&path_str).await?;
        let config = DatabaseConfig::new(format!("sqlite://{path_str}"));
        let pool = DatabasePool::new(backend, config);

        Ok(Self {
            path,
            pool,
            closed: false,
        })
    }

    /// Returns a reference to the database pool.
    pub fn pool(&self) -> &DatabasePool<rume_data::persistence::SqliteBackend> {
        &self.pool
    }

    /// Returns the file path of the temporary SQLite database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Begins an isolated transaction wrapped in a [`TestTransactionFixture`].
    pub async fn begin_test_tx(
        &self,
    ) -> Result<TestTransactionFixture<rume_data::persistence::SqliteTransaction>, PersistenceError>
    {
        let tx = self.pool.backend().begin_transaction().await?;
        Ok(TestTransactionFixture::new(tx))
    }

    /// Explicitly drains the pool and deletes temporary files, returning any errors to the test runner.
    pub async fn close(mut self) -> Result<(), PersistenceError> {
        self.closed = true;
        // 1. Drain and close connection pool
        self.pool.close_async().await;
        // 2. Remove files deterministically
        let path = self.path.clone();
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        if wal.exists() {
            let _ = std::fs::remove_file(wal);
        }
        if shm.exists() {
            let _ = std::fs::remove_file(shm);
        }
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
impl std::ops::Deref for SqliteTestFixture {
    type Target = DatabasePool<rume_data::persistence::SqliteBackend>;
    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

#[cfg(feature = "sqlite")]
impl std::fmt::Debug for SqliteTestFixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTestFixture")
            .field("path", &self.path)
            .field("closed", &self.closed)
            .finish()
    }
}

#[cfg(feature = "sqlite")]
impl Drop for SqliteTestFixture {
    fn drop(&mut self) {
        if !self.closed {
            let path = self.path.clone();
            let _ = std::fs::remove_file(format!("{}-wal", path.display()));
            let _ = std::fs::remove_file(format!("{}-shm", path.display()));
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// An isolated PostgreSQL schema (`test_<uuid>`) created per test and dropped with `CASCADE` on teardown.
///
/// # Teardown & Runtime Semantics
/// - Explicit teardown via [`Self::close`] is always preferred as it surfaces cleanup errors.
/// - On `multi_thread` Tokio runtimes: `Drop` uses `tokio::task::block_in_place` to synchronously
///   execute `DROP SCHEMA ... CASCADE` (5s timeout).
/// - On `current_thread` runtimes or outside Tokio: `Drop` aborts any unpolled background
///   rollback tasks from [`Self::begin_test_tx`], opens a fresh 1-connection pool on a joined
///   helper OS thread, terminates any lingering server backends scoped to this schema (releasing
///   table/schema locks immediately), and synchronously runs `DROP SCHEMA ... CASCADE` (5s timeout)
///   before `drop()` returns — never detaching schema cleanup onto the test runtime.
#[cfg(feature = "postgres")]
pub struct PostgresSchemaFixture {
    schema_name: String,
    pool: DatabasePool<rume_data::persistence::PostgresBackend>,
    admin_pool: Option<DatabasePool<rume_data::persistence::PostgresBackend>>,
    pending_rollbacks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    closed: bool,
}

#[cfg(feature = "postgres")]
impl PostgresSchemaFixture {
    /// Creates a unique schema in the target PostgreSQL instance and returns a pool scoped to that schema.
    pub async fn create(
        admin_pool: &DatabasePool<rume_data::persistence::PostgresBackend>,
    ) -> Result<Self, PersistenceError> {
        let schema_name = format!("test_{}", uuid::Uuid::new_v4().simple());

        admin_pool
            .backend()
            .execute(&format!("CREATE SCHEMA \"{schema_name}\""))
            .await?;

        let base_url = &admin_pool.config().url;
        let scoped_url = if base_url.contains('?') {
            format!(
                "{base_url}&application_name={schema_name}&options=-csearch_path%3D{schema_name}"
            )
        } else {
            format!(
                "{base_url}?application_name={schema_name}&options=-csearch_path%3D{schema_name}"
            )
        };

        let mut config = admin_pool.config().clone();
        config.url = scoped_url;
        let backend = rume_data::persistence::PostgresBackend::connect(&config).await?;
        let pool = DatabasePool::new(backend, config);

        Ok(Self {
            schema_name,
            pool,
            admin_pool: Some(admin_pool.clone()),
            pending_rollbacks: Arc::new(std::sync::Mutex::new(Vec::new())),
            closed: false,
        })
    }

    /// Returns the schema name created for this test run.
    pub fn schema_name(&self) -> &str {
        &self.schema_name
    }

    /// Returns a reference to the pool scoped to this test's isolated schema.
    pub fn pool(&self) -> &DatabasePool<rume_data::persistence::PostgresBackend> {
        &self.pool
    }

    /// Begins an isolated transaction wrapped in a [`TestTransactionFixture`].
    pub async fn begin_test_tx(
        &self,
    ) -> Result<TestTransactionFixture<rume_data::persistence::PostgresTransaction>, PersistenceError>
    {
        let tx = self.pool.backend().begin_transaction().await?;
        Ok(TestTransactionFixture::with_pending_rollbacks(
            tx,
            Arc::clone(&self.pending_rollbacks),
        ))
    }

    /// Explicitly drains the pool and drops the schema, returning any cleanup errors to the test runner.
    pub async fn close(mut self) -> Result<(), PersistenceError> {
        self.closed = true;
        if let Ok(mut list) = self.pending_rollbacks.lock() {
            for handle in list.drain(..) {
                handle.abort();
            }
        }
        self.pool.close_async().await;
        if let Some(admin) = self.admin_pool.take() {
            admin
                .backend()
                .execute(&format!(
                    "DROP SCHEMA IF EXISTS \"{}\" CASCADE",
                    self.schema_name
                ))
                .await?;
        }
        Ok(())
    }
}

#[cfg(feature = "postgres")]
impl std::ops::Deref for PostgresSchemaFixture {
    type Target = DatabasePool<rume_data::persistence::PostgresBackend>;
    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

#[cfg(feature = "postgres")]
impl std::fmt::Debug for PostgresSchemaFixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresSchemaFixture")
            .field("schema_name", &self.schema_name)
            .field("closed", &self.closed)
            .finish()
    }
}

#[cfg(feature = "postgres")]
impl Drop for PostgresSchemaFixture {
    fn drop(&mut self) {
        if !self.closed
            && let Some(admin) = self.admin_pool.take()
        {
            if let Ok(mut list) = self.pending_rollbacks.lock() {
                for handle in list.drain(..) {
                    handle.abort();
                }
            }
            let schema = self.schema_name.clone();
            let terminate_sql = format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() AND (application_name = '{schema}' OR pid IN (\
                 SELECT l.pid FROM pg_locks l \
                 JOIN pg_class c ON l.relation = c.oid \
                 JOIN pg_namespace n ON c.relnamespace = n.oid \
                 WHERE n.nspname = '{schema}'))"
            );
            let drop_sql = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
            let timeout = std::time::Duration::from_secs(5);
            match tokio::runtime::Handle::try_current() {
                Ok(handle)
                    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
                {
                    // Why: Multi-thread Tokio lets `block_in_place` block this thread while other
                    // worker threads keep driving `admin`'s existing socket reactor.
                    tokio::task::block_in_place(|| {
                        let _ = handle.block_on(tokio::time::timeout(timeout, async {
                            let _ = admin.backend().query_scalar(&terminate_sql).await;
                            let _ = admin.backend().execute(&drop_sql).await;
                        }));
                    });
                }
                _ => {
                    // Why: On `current_thread` runtimes (all `containers/postgres.rs` tests) or
                    // outside Tokio, `.join()`ing over `admin_pool`'s outer-reactor sockets would
                    // halt `epoll_wait` and deadlock, while `handle.spawn` detaches schema deletion
                    // from the test lifetime so runtime shutdown can discard it before completion.
                    // Clone `DatabaseConfig` (1 conn, 0 min) into a fresh `PostgresBackend` on an
                    // isolated helper thread's runtime, terminate any lingering server backends for
                    // this schema (such as an uncommitted transaction dropped immediately prior
                    // without an intervening `.await`), and execute `DROP SCHEMA ... CASCADE`
                    // synchronously before `drop()` returns.
                    let mut drop_cfg = admin.config().clone();
                    drop_cfg.max_connections = 1;
                    drop_cfg.min_connections = 0;
                    drop(admin);
                    let _ = std::thread::spawn(move || {
                        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            rt.block_on(async move {
                                let _ = tokio::time::timeout(timeout, async {
                                    if let Ok(backend) =
                                        rume_data::persistence::PostgresBackend::connect(&drop_cfg)
                                            .await
                                    {
                                        let _ = backend.query_scalar(&terminate_sql).await;
                                        let _ = backend.execute(&drop_sql).await;
                                        backend.close().await;
                                    }
                                })
                                .await;
                            });
                        }
                    })
                    .join();
                }
            }
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
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

            fn commit(
                &self,
            ) -> rume_data::persistence::BoxFuture<'_, Result<(), PersistenceError>> {
                Box::pin(async { Ok(()) })
            }

            fn rollback(
                &self,
            ) -> rume_data::persistence::BoxFuture<'_, Result<(), PersistenceError>> {
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

        entered_notified.await;
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
}
