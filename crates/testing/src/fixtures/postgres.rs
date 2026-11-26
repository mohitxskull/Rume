use super::*;

/// An isolated PostgreSQL schema (`test_<uuid>`) created per test and dropped with `CASCADE` on teardown.
///
/// # Teardown & Runtime Semantics
/// - Explicit teardown via [`Self::close`] is always preferred as it surfaces cleanup errors.
/// - On `multi_thread` Tokio runtimes: `Drop` uses `tokio::task::block_in_place` to synchronously
///   execute `DROP SCHEMA ... CASCADE` (5s timeout).
/// - On `current_thread` runtimes or outside Tokio: `Drop` opens a fresh 1-connection pool on a
///   joined helper OS thread, terminates any lingering server backends scoped to this schema
///   (releasing table/schema locks immediately), and synchronously runs `DROP SCHEMA ... CASCADE`
///   (5s timeout) before `drop()` returns — never detaching schema cleanup onto the test runtime.
#[cfg(feature = "postgres")]
pub struct PostgresSchemaFixture {
    schema_name: String,
    pool: DatabasePool<rume_data::persistence::PostgresBackend>,
    admin_pool: Option<DatabasePool<rume_data::persistence::PostgresBackend>>,
    closed: bool,
}

#[cfg(feature = "postgres")]
struct SchemaCreationGuard {
    admin_pool: Option<DatabasePool<rume_data::persistence::PostgresBackend>>,
    schema_name: String,
}

#[cfg(feature = "postgres")]
impl SchemaCreationGuard {
    fn disarm(mut self) {
        self.admin_pool = None;
    }
}

#[cfg(feature = "postgres")]
impl Drop for SchemaCreationGuard {
    fn drop(&mut self) {
        if let Some(admin) = self.admin_pool.take() {
            drop_schema_sync(admin, &self.schema_name);
        }
    }
}

#[cfg(feature = "postgres")]
fn drop_schema_sync(admin: DatabasePool<rume_data::persistence::PostgresBackend>, schema: &str) {
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
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
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
                                rume_data::persistence::PostgresBackend::connect(&drop_cfg).await
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

        // Why: Install a cleanup guard immediately after `CREATE SCHEMA` so that if
        // connecting the scoped pool fails or this future is cancelled, the newly
        // created schema is still dropped synchronously rather than leaked.
        let guard = SchemaCreationGuard {
            admin_pool: Some(admin_pool.clone()),
            schema_name: schema_name.clone(),
        };

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

        guard.disarm();

        Ok(Self {
            schema_name,
            pool,
            admin_pool: Some(admin_pool.clone()),
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
        Ok(TestTransactionFixture::new(tx))
    }

    /// Explicitly drains the pool and drops the schema, returning any cleanup errors to the test runner.
    pub async fn close(mut self) -> Result<(), PersistenceError> {
        // Why: SQLx `Pool::close()` waits indefinitely if a transaction returned by
        // `begin_test_tx()` is still alive. Bound the drain so `close()` returns a timeout
        // error instead of hanging, leaving `self.closed == false` so `Drop` still terminates
        // lingering server backends and drops the schema.
        tokio::time::timeout(std::time::Duration::from_secs(5), self.pool.close_async())
            .await
            .map_err(|_| {
                PersistenceError::ConnectionFailed(format!(
                    "timed out draining pool for schema '{}' (is a transaction still open?)",
                    self.schema_name
                ))
            })?;
        if let Some(admin) = self.admin_pool.as_ref() {
            admin
                .backend()
                .execute(&format!(
                    "DROP SCHEMA IF EXISTS \"{}\" CASCADE",
                    self.schema_name
                ))
                .await?;
        }
        // Why: Mark closed and release `admin_pool` only after `DROP SCHEMA` succeeds
        // so that if `DROP SCHEMA` fails or `close()` is cancelled, `Drop` still runs
        // its fallback cleanup.
        self.closed = true;
        self.admin_pool = None;
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
            drop_schema_sync(admin, &self.schema_name);
        }
    }
}
