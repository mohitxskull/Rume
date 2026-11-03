# Persistence & Pool Lifecycle Review — v03

## Summary

The modernized persistence subsystem introduces asynchronous SQLx 0.8 connection pools across PostgreSQL, MySQL/MariaDB, and SQLite, alongside transactional outbox integration and schema migrations. However, the connection pool lifecycle abstraction is fundamentally compromised: `DatabasePool` operates as a disconnected "potemkin" layer whose shadow semaphore and fabricated counters mask the true state of the underlying database, while `close()` and `close_and_drain()` return false shutdown success without ever closing physical SQLx connection pools or in-flight backend queries. Furthermore, critical pool configuration properties (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`, `auto_migrate`) are silently discarded across all backends, health checks risk hanging during connection starvation, and a schema divergence in migration tracking guarantees failure upon initialization. The persistence subsystem is not ready for production without a targeted remediation pass.

---

## Prior decisions

In accordance with `.opencode/skills/review-authoring/SKILL.md`, prior findings and architectural decisions from `v02` intersecting with persistence, transactions, and pool lifecycle have been audited against the modernized codebase:

- **Correctness M3 (`Migrator::rollback_last` silently unmarks migrations when `down_sql` is `None`)** — **REAFFIRMED**. Verified in `rume/src/persistence/migration.rs:208–212`. When `migration.down_sql.is_none()`, `rollback_last_backend` fails closed by returning `Err(PersistenceError::IrreversibleMigration(migration.id))` and preserves the tracking record in `rume_migrations`.
- **Positive Note v02 (`MemoryTransaction` automatic rollback on drop)** — **REAFFIRMED**. Verified across SQLx backends in `rume/tests/sqlite_suite.rs:126–140` and `rume/tests/mysql_container_suite.rs:118–144`. Dropping a transaction handle (`PostgresTransaction`, `MysqlTransaction`, `SqliteTransaction`) without an explicit `commit()` delegates to `sqlx::Transaction` drop semantics, successfully rolling back uncommitted mutations.
- **Enterprise-Reliability C2 / Group B (`TransactionalOutbox::save_tx` transactional participation)** — **REAFFIRMED**. In v02, `OutboxRepository` lacked transactional atomicity. In v03, `TransactionalOutbox<B::Transaction>` is implemented across `PostgresBackend`, `MysqlBackend`, `SqliteBackend`, and `DatabasePool`, ensuring outbox messages can be written within active database transactions.
- **Connection pool abstraction delegation (`DatabasePool` vs `sqlx::Pool`)** — **NOTE-NEW**. Connection pooling and backend abstraction were modernized to SQLx 0.8 in commit `819a857`. Evaluated in findings C1, C2, and M1.
- **Pool configuration forwarding (`DatabaseConfig`)** — **NOTE-NEW**. Configuration options (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`) were expanded in v03. Evaluated in finding M1.
- **Physical connection health checks & ping robustness** — **NOTE-NEW**. `DatabaseHealthCheck` and `ping()` across driver backends were introduced in v03. Evaluated in finding M2.

---

## Findings

### C1 False shutdown success and physical connection leak in `DatabasePool::close()` and `close_and_drain()`

- **Where:** `rume/src/persistence/pool.rs:187–208`, `rume/src/persistence/backend.rs:13–28`, `rume/src/persistence/postgres.rs:26–37`, `rume/src/persistence/mysql.rs:26–43`, `rume/src/persistence/sqlite.rs:28–39`
- **What it says:**
  In `rume/src/persistence/pool.rs`:
  ```rust
  pub fn close(&self) {
      self.is_closed.store(true, Ordering::SeqCst);
      self.semaphore.close();
      self.total_connections.store(0, Ordering::Relaxed);
  }

  pub async fn close_and_drain(&self, timeout: Duration) -> bool {
      self.close();
      let start = tokio::time::Instant::now();
      while self.active_connections.load(Ordering::SeqCst) > 0
          || self.in_flight_transactions.load(Ordering::SeqCst) > 0
      {
          if start.elapsed() >= timeout {
              return false;
          }
          tokio::time::sleep(Duration::from_millis(10)).await;
      }
      true
  }
  ```
- **Why it's a problem:**
  1. **Physical pool is never closed:** `DatabasePool::close()` modifies only its internal atomic flags and local semaphore; it **never calls `close()` on the underlying SQLx pool** (`sqlx::PgPool`, `sqlx::MySqlPool`, or `sqlx::SqlitePool`). The `DatabaseBackend` trait does not even define a `close()` method. Physical TCP sockets to PostgreSQL/MySQL and open SQLite OS file locks remain fully open.
  2. **In-flight backend queries are invisible to drain:** Subsystems throughout Rume (`PostgresJobQueue`, `PostgresDistributedLock`, `PostgresLeaderElector`, `Migrator`, and user query handlers) execute directly via `pool.backend()`. These operations do not acquire `ConnectionLease` permits or increment `in_flight_transactions`.
  3. **False shutdown success:** When `close_and_drain()` runs during application shutdown, it checks only `active_connections > 0` and `in_flight_transactions > 0`. Because queries executed via `pool.backend()` never touched these counters, `close_and_drain()` immediately observes `0` and returns `true`, falsely signaling clean termination to the shutdown supervisor.
  4. **Abrupt connection termination and data loss:** The server proceeds to terminate the process while active queries, outbox saves, or background job writes are still running over physical TCP connections, resulting in severed sockets, broken pipe errors, and orphaned server-side transaction locks.
- **Suggested fix:**
  1. Add `close(&self) -> BoxFuture<'_, ()>` and `is_closed(&self) -> bool` to `DatabaseBackend`.
  2. Implement `close()` on `PostgresBackend`, `MysqlBackend`, and `SqliteBackend` to call `self.pool.close().await`.
  3. In `DatabasePool::close()`, trigger backend closure. In `DatabasePool::close_and_drain()`, await `self.backend.close().await` within the specified timeout to guarantee that all physical connections and in-flight operations are terminated.

---

### C2 Disconnected "potemkin" concurrency abstraction: `DatabasePool` maintains fictional metrics and a shadow semaphore disconnected from SQLx

- **Where:** `rume/src/persistence/pool.rs:31–39, 66–78, 91–98, 107–123`, `rume/src/persistence/backend.rs:13–28`
- **What it says:**
  In `rume/src/persistence/pool.rs`:
  ```rust
  pub struct DatabasePool<B: DatabaseBackend> {
      backend: Arc<B>,
      config: DatabaseConfig,
      active_connections: Arc<AtomicU32>,
      total_connections: Arc<AtomicU32>,
      in_flight_transactions: Arc<AtomicU32>,
      semaphore: Arc<Semaphore>,
      is_closed: Arc<AtomicBool>,
  }
  ...
  pub fn new(backend: B, config: DatabaseConfig) -> Self {
      let initial_conns = config.min_connections;
      let max_conns = config.max_connections.max(1) as usize;
      Self {
          backend: Arc::new(backend),
          config,
          active_connections: Arc::new(AtomicU32::new(0)),
          total_connections: Arc::new(AtomicU32::new(initial_conns)),
          in_flight_transactions: Arc::new(AtomicU32::new(0)),
          semaphore: Arc::new(Semaphore::new(max_conns)),
          is_closed: Arc::new(AtomicBool::new(false)),
      }
  }
  ```
- **Why it's a problem:**
  `DatabasePool` maintains a parallel, fictional state machine that is completely decoupled from the actual connection pool managed by SQLx:
  1. **Fabricated connection counters:** `total_connections` is initialized to `config.min_connections` and is never updated until `close()` zeroes it. If SQLx grows the connection pool to 10 connections, `total_connections` still reads `1`. `active_connections` only increments if an external caller explicitly calls `acquire_lease()` or `pool.transaction()`.
  2. **Complete abstraction bypass:** `DatabasePool` does not implement `DatabaseBackend`. Components like `PostgresJobQueue::from_backend(pool.backend())`, `PostgresDistributedLock::from_backend(pool.backend())`, `Migrator::run_pending(pool.backend())`, and any caller running `pool.backend().execute(...)` completely bypass the semaphore and connection counters. Under heavy background job load, dozens of connections can be checked out of SQLx while `pool.status().active_connections` continues to report `0`.
  3. **Misleading telemetry and readiness checks:** In `DatabaseHealthCheck::run()`, `meta` reports `active_connections` and `total_connections` from `pool.status()`. Prometheus dashboards and Kubernetes readiness probes receive entirely fictitious connection metrics.
  4. **False pool exhaustion and starvation races:** A client acquiring permits via `acquire_lease()` can receive `PersistenceError::PoolExhausted` because the shadow semaphore has 0 permits, even though the underlying database pool is completely idle. Conversely, `acquire_lease()` can succeed because the shadow semaphore has permits, only for the subsequent database query or `begin_transaction()` to block or fail because the physical SQLx pool is exhausted by direct `pool.backend()` queries.
- **Suggested fix:**
  Eliminate the shadow semaphore and fictional atomic counters. Delegate connection metrics and pool management directly to the underlying driver:
  1. Expose pool telemetry via `DatabaseBackend`: `fn pool_size(&self) -> u32`, `fn pool_idle(&self) -> u32`, `fn is_closed(&self) -> bool`.
  2. Query `self.backend.pool().size()` and `self.backend.pool().num_idle()` in `PoolStatus`.
  3. If lease tracking is required, implement `ConnectionLease` as a wrapper around `sqlx::pool::PoolConnection` rather than an empty permit over a detached semaphore.

---

### M1 Critical pool configuration properties (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`, `auto_migrate`) are silently ignored by backends

- **Where:** `rume/src/persistence/postgres.rs:44–62`, `rume/src/persistence/mysql.rs:50–68`, `rume/src/persistence/sqlite.rs:50–57, 73–89`, `rume/src/persistence/config.rs:20–43`
- **What it says:**
  In `rume/src/persistence/postgres.rs`:
  ```rust
  pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
      let pool = PgPoolOptions::new()
          .max_connections(config.max_connections.max(1))
          .connect_lazy(&config.url)
          .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

      Ok(Self { pool })
  }
  ```
  In `rume/src/persistence/mysql.rs`:
  ```rust
  pub fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
      let pool = MySqlPoolOptions::new()
          .max_connections(config.max_connections.max(1))
          .connect_lazy(&config.url)
          .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;

      Ok(Self { pool })
  }
  ```
  In `rume/src/persistence/sqlite.rs`:
  ```rust
  pub async fn open(path: &str) -> Result<Self, PersistenceError> {
      ...
      let pool = SqlitePoolOptions::new()
          .max_connections(10)
          .connect_with(opts)
          .await
          .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
      Ok(Self { pool })
  }

  pub async fn new(config: &DatabaseConfig) -> Result<Self, PersistenceError> {
      let url = config.url.trim();
      if url == "sqlite::memory:" || url == ":memory:" || url == "memory://" || url.is_empty() {
          Self::in_memory().await
      } else {
          Self::open(url).await
      }
  }
  ```
- **Why it's a problem:**
  1. **Ignored pool bounds in Postgres and MySQL:** `DatabaseConfig` explicitly defines `min_connections`, `idle_timeout_secs`, `connect_timeout_secs`, and `auto_migrate`. In `PostgresBackend` and `MysqlBackend`, only `max_connections` is configured on `PgPoolOptions` / `MySqlPoolOptions`. `min_connections` is ignored (no warm connections maintained), `idle_timeout_secs` is ignored (idle connections never pruned, causing stateful NAT/firewall drops), and `connect_timeout_secs` is ignored (`acquire_timeout` defaults to SQLx's 30s instead of the configured timeout).
  2. **Hardcoded limits and total config discard in SQLite:** `SqliteBackend::new` ignores `config.max_connections` entirely. `open()` hardcodes `max_connections(10)`. If a developer configures `DatabaseConfig::new("sqlite://app.db").with_max_connections(2)`, `SqliteBackend` opens 10 connections, while `DatabasePool` creates a semaphore for 2, creating mismatched capacity limits.
  3. **Dead configuration options:** `auto_migrate: bool` is parsed and accepted in `DatabaseConfig` but is never checked or invoked by any backend or application bootloader.
- **Suggested fix:**
  Forward all configuration fields to the underlying SQLx pool options across all backends:
  ```rust
  let mut opts = PgPoolOptions::new()
      .max_connections(config.max_connections.max(1))
      .min_connections(config.min_connections)
      .acquire_timeout(config.connect_timeout());
  if let Some(idle) = config.idle_timeout() {
      opts = opts.idle_timeout(idle);
  }
  ```
  Apply the same pattern to `MySqlPoolOptions` and `SqlitePoolOptions`.

---

### M2 `DatabaseHealthCheck` and `ping()` hang during pool saturation or network partition due to missing acquisition timeouts

- **Where:** `rume/src/persistence/pool.rs:265–307`, `rume/src/persistence/postgres.rs:106–114`, `rume/src/persistence/mysql.rs:115–123`, `rume/src/persistence/sqlite.rs:137–145`
- **What it says:**
  In `rume/src/persistence/pool.rs`:
  ```rust
  match pool.backend().ping().await {
      Ok(()) => HealthCheckResult {
          name,
          status: HealthStatus::Ok,
          message: "database connection healthy".to_string(),
          meta,
          cached: false,
      },
      Err(err) => HealthCheckResult {
          name,
          status: HealthStatus::Failed,
          message: format!("database connection check failed: {err}"),
          meta,
          cached: false,
      },
  }
  ```
  In `rume/src/persistence/postgres.rs`:
  ```rust
  fn ping(&self) -> BoxFuture<'_, Result<(), PersistenceError>> {
      Box::pin(async move {
          sqlx::query("SELECT 1")
              .execute(&self.pool)
              .await
              .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))?;
          Ok(())
      })
  }
  ```
- **Why it's a problem:**
  1. **No timeout on health check invocation:** `DatabaseHealthCheck::run` awaits `pool.backend().ping().await` with no `tokio::time::timeout` wrapper.
  2. **Pool saturation stalls health checks:** `sqlx::query("SELECT 1").execute(&self.pool)` must acquire an available connection from `&self.pool`. If all connections are in use by long-running transactions or queries, the ping query waits up to SQLx's default `acquire_timeout` (30 seconds).
  3. **Probe death spiral in Kubernetes:** Kubernetes liveness and readiness probes typically enforce a 1 to 5 second deadline. During a traffic surge where the database pool is saturated, health check requests will block for up to 30 seconds, exceeding probe timeouts. Kubernetes flags the pod unready or terminates it, exacerbating the outage.
  4. **Dropped TCP connections:** If an idle connection has been dropped by a firewall or cloud load balancer, executing against the pool without a bounded socket timeout can stall until the OS TCP retransmission budget expires.
- **Suggested fix:**
  1. Wrap `ping()` in an explicit, tight timeout (e.g. `Duration::from_secs(3)` or `config.connect_timeout()`):
     ```rust
     tokio::time::timeout(Duration::from_secs(3), async {
         sqlx::query("SELECT 1")
             .execute(&self.pool)
             .await
             .map_err(|e| PersistenceError::ConnectionFailed(e.to_string()))
     })
     .await
     .map_err(|_| PersistenceError::Timeout)??;
     ```
  2. In `DatabaseHealthCheck::run`, guard the ping execution with a timeout and return `HealthStatus::Failed` with a descriptive message if the deadline expires.

---

### M3 Schema divergence: `init_migrations_table()` creates `rume_migrations` with `NOT NULL` columns (`name`, `checksum`), causing `Migrator::run_pending` to fail

- **Where:** `rume/src/persistence/migration.rs:98–102, 150–154, 177–182`, `rume/src/persistence/sql/postgres/migrations_schema.sql:1–6`, `rume/src/persistence/sql/mysql/migrations_schema.sql:1–6`, `rume/src/persistence/sql/sqlite/migrations_schema.sql:1–6`, `rume/src/persistence/postgres.rs:81–84`, `rume/src/persistence/mysql.rs:87–90`, `rume/src/persistence/sqlite.rs:109–112`
- **What it says:**
  In `sql/postgres/migrations_schema.sql`, `sql/mysql/migrations_schema.sql`, and `sql/sqlite/migrations_schema.sql`:
  ```sql
  CREATE TABLE IF NOT EXISTS rume_migrations (
      id VARCHAR(255) PRIMARY KEY,
      name VARCHAR(255) NOT NULL,
      applied_at BIGINT NOT NULL,
      checksum VARCHAR(64) NOT NULL
  );
  ```
  `PostgresBackend::init_migrations_table()`, `MysqlBackend::init_migrations_table()`, and `SqliteBackend::init_migrations_table()` execute `MIGRATIONS_SCHEMA`.
  However, in `rume/src/persistence/migration.rs`:
  ```rust
  backend
      .execute(
          "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
      )
      .await?;
  ...
  let insert_sql = format!(
      "INSERT INTO rume_migrations (id, applied_at) VALUES ('{}', {})",
      migration.id.replace('\'', "''"),
      now
  );
  backend.execute(&insert_sql).await?;
  ```
- **Why it's a problem:**
  1. **Immediate SQL runtime failure on initialized schemas:** If an application calls `backend.init_migrations_table()` during setup or if a DBA runs the provided DDL scripts, `rume_migrations` is created with `name NOT NULL` and `checksum NOT NULL` without default values.
  2. When `Migrator::run_pending` executes, it runs `INSERT INTO rume_migrations (id, applied_at) VALUES (...)`. This insert violates the NOT NULL constraints on `name` and `checksum`.
  3. PostgreSQL aborts with `null value in column "name" of relation "rume_migrations" violates not-null constraint`. MySQL aborts with `Field 'name' doesn't have a default value`. The application crashes during boot or migration preflight.
  4. Furthermore, the inline DDL in `migration.rs` contradicts the canonical SQL schema files included in `src/persistence/sql/`.
- **Suggested fix:**
  1. Update `Migrator::run_pending_backend` to compute `checksum` (e.g. SHA-256 of `up_sql.as_deref().unwrap_or("")`) and populate `name` (from `migration.description`).
  2. Align the inline fallback DDL in `migration.rs:100, 152, 197, 241` to include `name` and `checksum`.
  3. Update `insert_sql` to supply all four columns:
     ```rust
     let insert_sql = format!(
         "INSERT INTO rume_migrations (id, name, applied_at, checksum) VALUES ('{}', '{}', {}, '{}')",
         migration.id.replace('\'', "''"),
         migration.description.replace('\'', "''"),
         now,
         checksum
     );
     ```

---

### M4 Non-atomic migration application and lack of distributed migration lock in HA environments

- **Where:** `rume/src/persistence/migration.rs:164–184, 214–227`
- **What it says:**
  In `rume/src/persistence/migration.rs`:
  ```rust
  if let Some(sql) = &migration.up_sql {
      backend
          .execute(sql)
          .await
          .map_err(|err| PersistenceError::MigrationError {
              migration: migration.id.clone(),
              message: err.to_string(),
          })?;
  }
  let now = std::time::SystemTime::now()...;
  let insert_sql = format!(
      "INSERT INTO rume_migrations (id, applied_at) VALUES ('{}', {})",
      migration.id.replace('\'', "''"),
      now
  );
  backend.execute(&insert_sql).await?;
  ```
  and `rollback_last_backend`:
  ```rust
  backend
      .execute(sql)
      .await
      .map_err(|err| PersistenceError::MigrationError { ... })?;

  let delete_sql = format!(
      "DELETE FROM rume_migrations WHERE id = '{}'",
      migration.id.replace('\'', "''")
  );
  backend.execute(&delete_sql).await?;
  ```
- **Why it's a problem:**
  1. **Schema inconsistency on crash:** DDL execution (`backend.execute(sql)`) and recording the applied migration (`backend.execute(&insert_sql)`) are two separate, non-transactional statements. If the process is terminated (e.g. OOM kill, network drop, node reboot) immediately after `backend.execute(sql)` succeeds, the migration script ran, but `rume_migrations` has no record of it. On restart, `run_pending` attempts to re-execute the migration, causing "relation already exists" or duplicate column errors.
  2. **Rollback corruption:** The identical failure mode exists in `rollback_last_backend`: if `down_sql` drops a table but the process terminates before `DELETE FROM rume_migrations`, the database is left missing the table while the tracking table records it as applied.
  3. **Concurrent migration races:** When multiple application pods start concurrently in a clustered environment, all pods call `run_pending` simultaneously. Without an advisory lock or distributed lock, multiple pods execute DDL statements in parallel, causing race errors and conflicting migration records.
- **Suggested fix:**
  1. Enclose transactional DDL and the migration record insertion within an explicit `DatabaseTransaction` (`backend.begin_transaction().await?`), committing both together (supported natively in PostgreSQL and SQLite; noted for MySQL DDL which causes implicit commits).
  2. For multi-replica deployments, acquire an advisory lock (e.g. `pg_advisory_lock` in PostgreSQL or `GET_LOCK` in MySQL) before inspecting pending migrations and running DDL.

---

### M5 Error variant flattening: SQLx errors are unconditionally mapped to `TransactionError` or `QueryFailed`, masking `PoolExhausted`, `Timeout`, and `Closed`

- **Where:** `rume/src/persistence/postgres.rs:109–112, 122, 133, 145`, `rume/src/persistence/mysql.rs:118–121, 131, 142, 154`, `rume/src/persistence/sqlite.rs:140–143, 153, 164, 176`
- **What it says:**
  Across `PostgresBackend`, `MysqlBackend`, and `SqliteBackend`:
  ```rust
  fn execute(&self, statement: &str) -> BoxFuture<'_, Result<u64, PersistenceError>> {
      ...
      sqlx::raw_sql(&stmt)
          .execute(&self.pool)
          .await
          .map_err(|e| PersistenceError::QueryFailed(e.to_string()))?;
      ...
  }

  fn begin_transaction(&self) -> BoxFuture<'_, Result<Self::Transaction, PersistenceError>> {
      ...
      let tx = self
          .pool
          .begin()
          .await
          .map_err(|e| PersistenceError::TransactionError(e.to_string()))?;
      ...
  }
  ```
- **Why it's a problem:**
  `PersistenceError` defines specific, actionable failure variants: `PoolExhausted`, `Timeout`, `Closed`, `ConnectionFailed`, `TransactionError`, and `QueryFailed`.
  However, all backend implementations bypass these variants and unconditionally map `sqlx::Error` to `QueryFailed` or `TransactionError`:
  1. When SQLx times out acquiring a connection (`sqlx::Error::PoolTimedOut`), `begin_transaction()` wraps it in `PersistenceError::TransactionError("pool timed out")`.
  2. When queries fail because the pool has been shut down (`sqlx::Error::PoolClosed`), it is wrapped in `PersistenceError::QueryFailed("pool is closed")`.
  3. Upstream callers, resilience middleware (circuit breakers, retry policies), and supervisors that match on `PersistenceError::Timeout` or `PersistenceError::Closed` will never match on driver errors, because the variants are flattened into generic strings.
- **Suggested fix:**
  Implement a unified error mapping function:
  ```rust
  pub(crate) fn map_sqlx_error(err: sqlx::Error) -> PersistenceError {
      match err {
          sqlx::Error::PoolTimedOut => PersistenceError::Timeout,
          sqlx::Error::PoolClosed => PersistenceError::Closed,
          sqlx::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => PersistenceError::Timeout,
          sqlx::Error::Database(e) => PersistenceError::QueryFailed(e.to_string()),
          other => PersistenceError::QueryFailed(other.to_string()),
      }
  }
  ```
  Use this helper consistently across all backend queries and transaction initializations.

---

### m1 Multi-statement execution failure in `MysqlBackend::execute`, `SqliteBackend::execute`, and `PostgresTransaction::execute` due to `sqlx::query` vs `sqlx::raw_sql`

- **Where:** `rume/src/persistence/mysql.rs:128`, `rume/src/persistence/sqlite.rs:150`, `rume/src/persistence/postgres.rs:174`
- **What it says:**
  In `PostgresBackend::execute`:
  ```rust
  let res = sqlx::raw_sql(&stmt).execute(&self.pool).await...
  ```
  In `MysqlBackend::execute`:
  ```rust
  let res = sqlx::query(&stmt).execute(&self.pool).await...
  ```
  In `SqliteBackend::execute`:
  ```rust
  let res = sqlx::query(&stmt).execute(&self.pool).await...
  ```
  In `PostgresTransaction::execute`:
  ```rust
  let res = sqlx::query(&stmt).execute(&mut **tx).await...
  ```
- **Why it's a problem:**
  `sqlx::query(&stmt)` prepares a single SQL statement using the database's extended query protocol. If a caller submits a script containing multiple semicolon-separated statements (e.g. a DDL migration script with multiple `CREATE TABLE` statements or indexes), `sqlx::query` fails with a protocol syntax error in MySQL and SQLite. `PostgresBackend::execute` correctly used `sqlx::raw_sql`, but `MysqlBackend`, `SqliteBackend`, and `PostgresTransaction` used `sqlx::query`.
- **Suggested fix:**
  Use `sqlx::raw_sql(&stmt)` consistently across `MysqlBackend::execute`, `SqliteBackend::execute`, and `PostgresTransaction::execute`.

---

### m2 Inconsistent transaction auto-commit collision in `DatabasePool::transaction`

- **Where:** `rume/src/persistence/pool.rs:174–183`
- **What it says:**
  In `rume/src/persistence/pool.rs`:
  ```rust
  match f(tx).await {
      Ok(value) => {
          tx_handle.commit().await?;
          Ok(value)
      }
      Err(err) => {
          let _ = tx_handle.rollback().await;
          Err(PersistenceError::from(err))
      }
  }
  ```
- **Why it's a problem:**
  If the closure `f(tx)` explicitly calls `tx.commit().await` during its execution and then returns `Ok(result)`, `DatabasePool::transaction` attempts to invoke `tx_handle.commit().await?` on line 176. Because `PostgresTransaction::commit` takes the inner transaction out of the mutex guard (`guard.take()`), the second commit fails with `PersistenceError::TransactionError("transaction already resolved")`. The transaction successfully committed all mutations, but the outer helper falsely reports an error to the caller.
- **Suggested fix:**
  In `PostgresTransaction::commit`, `MysqlTransaction::commit`, and `SqliteTransaction::commit`, treat `guard.is_none()` as a successful no-op if the transaction was already committed, or document explicitly on `DatabasePool::transaction` that callers must not invoke `commit()` or `rollback()` within the closure.

---

## Positive notes

- **Strict isolation for MySQL row claiming:** In `rume/src/persistence/mysql.rs:326–333`, `claim_pending_with_lease` explicitly sets `SET TRANSACTION ISOLATION LEVEL READ COMMITTED` on the acquired connection before beginning the claiming transaction. This correctly prevents MySQL's default `REPEATABLE READ` snapshot isolation from seeing stale outbox rows during concurrent worker sweeps.
- **Comprehensive outbox integration across drivers:** All three relational drivers (`PostgresBackend`, `MysqlBackend`, `SqliteBackend`) implement the complete `OutboxRepository` interface and `TransactionalOutbox` contract with driver-native SQL scripts, enabling transactional outbox patterns across heterogeneous databases.
- **Fail-closed irreversible migration guard:** `Migrator::rollback_last_backend` (`rume/src/persistence/migration.rs:208–212`) strictly enforces that migrations lacking `down_sql` reject rollback attempts with `PersistenceError::IrreversibleMigration`, preventing silent de-registration of un-reverted database schema changes.
- **Safe transaction rollback on drop:** In `SqliteTransaction`, `MysqlTransaction`, and `PostgresTransaction`, transaction handles wrap `sqlx::Transaction` in an `Arc<tokio::sync::Mutex<Option<...>>>`. If a task panics or drops the handle before calling `commit()`, SQLx's drop handler automatically rolls back the in-flight transaction, preventing leaked transaction locks.
- **Idempotent migration runner:** `Migrator::run_pending_backend` inspects existing records in `rume_migrations` prior to applying each step, ensuring subsequent runs are idempotent and safe to invoke on every application startup.

---

## Verdict

**Not ready for production without a remediation pass.**

While the driver integrations (`PostgresBackend`, `MysqlBackend`, `SqliteBackend`) provide solid raw query execution and outbox capabilities via SQLx, the persistence subsystem contains **2 Critical findings** and **5 Major findings** that compromise operational reliability, pool lifecycle management, and schema integrity:

1. **C1** — `DatabasePool::close()` and `close_and_drain()` never close physical SQLx pools or wait for in-flight backend queries, returning false shutdown success while connections are leaked.
2. **C2** — `DatabasePool` operates a disconnected shadow semaphore and fake connection counters that do not govern backend queries and report fictitious metrics to monitoring systems.
3. **M1** — Connection pool configuration settings (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`, `auto_migrate`) are silently ignored by driver backends.
4. **M2** — Health checks and `ping()` queries lack acquisition timeouts and can hang for 30+ seconds during pool saturation, triggering false Kubernetes probe failures.
5. **M3** — `init_migrations_table()` creates `rume_migrations` with `NOT NULL` columns that cause `Migrator::run_pending` to fail with database constraint errors.
6. **M4** — Migration execution and recording are non-atomic, risking corrupted migration state upon process termination.
7. **M5** — SQLx error variants are flattened to generic strings, masking timeouts and pool closure from upstream error handlers.

Resolving these findings will ensure that Rume's database connection pooling, lifecycle management, and migration execution are sound and resilient under production enterprise workloads.
