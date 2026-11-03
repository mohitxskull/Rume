# Persistence & Pool Lifecycle Diff Review — v03 (v01)

## Summary

This diff review evaluates the pool lifecycle and persistence subsystem in Rume across commits `cf82559..HEAD`, auditing `rume/src/persistence/` (`pool.rs`, `backend.rs`, `migration.rs`, `postgres.rs`, `mysql.rs`, `sqlite.rs`), SQL migration schemas, and integration/container test suites. Major architectural improvements have successfully landed: physical SQLx connection pool closure and graceful draining are now wired through `DatabaseBackend::close()`, fictional connection metrics in `DatabasePool::status()` have been replaced with driver pool telemetry, SQLx pool options (`min_connections`, `idle_timeout`, `acquire_timeout`) are correctly forwarded across all relational backends, structured error variant mapping (`PoolClosed`, `PoolTimedOut`, `ConnectionFailed`) is uniformly applied, and transactions now support multi-statement raw SQL execution and idempotent commit/rollback. However, the change **fails** diff review and cannot be accepted as-is because the remediation for migration schema alignment (v03 M3) was incomplete: `Migrator::pending_migrations()` and `Migrator::verify_no_pending_backend()` still execute obsolete DDL creating `rume_migrations` without `name` and `checksum` columns, causing subsequent `run_pending()` executions on fresh databases to crash with SQL column errors and breaking `test_postgres_migrations_up_and_rollback` in the container integration test suite (Finding C1).

---

## Prior decisions

- **v03 Pool C1 (False shutdown success and physical connection leak in `DatabasePool::close()` and `close_and_drain()`)** — **RESOLVED**.
  `DatabaseBackend` now declares `pool_size()`, `pool_idle()`, `is_closed()`, and `close() -> BoxFuture<'_, ()>` in `rume/src/persistence/backend.rs:30–47`. `PostgresBackend`, `MysqlBackend`, and `SqliteBackend` implement `close()` to call `self.pool.close().await` and `is_closed()` querying `self.pool.is_closed()`. `DatabasePool::close()` triggers backend pool closure asynchronously via `tokio::spawn`, while `DatabasePool::close_and_drain()` loops over active permits/transactions and then awaits `self.backend.close()` with the remaining timeout budget (`rume/src/persistence/pool.rs:202–221`). Verified in `rume/tests/integration/persistence.rs:315–360` (`pool_acquire_timeout_and_graceful_drain`).

- **v03 Pool C2 (Disconnected "potemkin" concurrency abstraction: `DatabasePool` maintains fictional metrics and a shadow semaphore disconnected from SQLx)** — **RESOLVED**.
  In `rume/src/persistence/pool.rs:91–102`, `PoolStatus::total_connections` is no longer a static atomic initialized to `min_connections`; it dynamically queries `self.backend.pool_size()`, which queries `self.pool.size()` on PostgreSQL (`postgres.rs:170`), MySQL (`mysql.rs:179`), and SQLite (`sqlite.rs:195`). `PoolStatus::is_closed` combines pool and driver closure states (`self.is_closed.load(...) || self.backend.is_closed()`). Furthermore, all `OutboxRepository` operations implemented on `DatabasePool<B>` (`pool.rs:337–471`) now acquire permits via `self.acquire_lease()`, preventing background outbox sweeps from bypassing pool admission limits.

- **v03 Pool M1 (Critical pool configuration properties (`min_connections`, `idle_timeout_secs`, `connect_timeout_secs`, `auto_migrate`) are silently ignored by backends)** — **RESOLVED**.
  In `PostgresBackend::new`/`connect` (`postgres.rs:45–73`), `MysqlBackend::new`/`connect` (`mysql.rs:51–79`), and `SqliteBackend::new`/`open` (`sqlite.rs:60–98`), SQLx pool options now forward `max_connections(config.max_connections.max(1))`, `min_connections(config.min_connections)`, `acquire_timeout(config.connect_timeout())`, and `idle_timeout(idle)`. SQLite `open()` now routes through `DatabaseConfig` instead of hardcoding 10 connections. (Note: `DatabaseConfig::auto_migrate` remains an unread configuration field, tracked as minor finding m1).

- **v03 Pool M2 (`DatabaseHealthCheck` and `ping()` hang during pool saturation or network partition due to missing acquisition timeouts)** — **RESOLVED**.
  `ping()` across `PostgresBackend` (`postgres.rs:122–132`), `MysqlBackend` (`mysql.rs:131–141`), and `SqliteBackend` (`sqlite.rs:147–157`) now executes inside a strict 3-second `tokio::time::timeout`, returning `PersistenceError::Timeout` if connection acquisition or `SELECT 1` hangs. In addition, `DatabaseHealthCheck::run()` (`pool.rs:304–328`) wraps `ping()` in `tokio::time::timeout(pool.config().connect_timeout())`, returning `HealthStatus::Failed` with an explicit timeout error message instead of stalling readiness probes.

- **v03 Pool M3 (Schema divergence: `init_migrations_table()` creates `rume_migrations` with `NOT NULL` columns, causing `Migrator::run_pending` to fail)** — **CHALLENGED (Defective Remediation)**.
  Commit `5fc83e8` updated `rume/src/persistence/sql/*/migrations_schema.sql` to specify `DEFAULT ''` on `name` and `checksum`, and updated `Migrator::run_pending_backend` (`migration.rs:152, 186`) and `rollback_last_backend` (`migration.rs:208`) to create and populate all 4 columns. However, `Migrator::pending_migrations` (`migration.rs:100`) and `Migrator::verify_no_pending_backend` (`migration.rs:253`) were left executing the obsolete 2-column DDL (`id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL`). When preflight verification runs on a fresh database, it creates the truncated table, causing subsequent `run_pending()` executions to fail with `column "name" of relation "rume_migrations" does not exist`. Verified empirically by the failure of `test_postgres_migrations_up_and_rollback` in `rume/tests/containers/postgres.rs:119`. Reported as Critical finding C1.

- **v03 Pool M4 (Non-atomic migration application and lack of distributed migration lock in HA environments)** — **RESOLVED**.
  In `rume/src/persistence/migration.rs:164–197` and `225–241`, both migration script execution (`tx.execute(sql)`) and tracking table updates (`tx.execute(&insert_sql)` / `tx.execute(&delete_sql)`) are executed inside an explicit transaction (`let tx = backend.begin_transaction().await?`). If either the DDL script or the tracking insert/delete fails, the transaction is automatically rolled back, guaranteeing state atomicity in PostgreSQL and SQLite. (Multi-pod distributed locks for MySQL remain an operational clustering concern due to MySQL implicit DDL commits, but driver-level atomicity is resolved).

- **v03 Pool M5 (Error variant flattening: SQLx errors are unconditionally mapped to `TransactionError` or `QueryFailed`, masking `PoolExhausted`, `Timeout`, and `Closed`)** — **RESOLVED**.
  `map_sqlx_error` was introduced in `rume/src/persistence/backend.rs:51–58`, mapping `sqlx::Error::PoolTimedOut` to `PersistenceError::Timeout`, `sqlx::Error::PoolClosed` to `PersistenceError::Closed`, and `sqlx::Error::Io` to `PersistenceError::ConnectionFailed`. This mapping is consistently invoked in `ping()`, `execute()`, `query_scalar()`, and `begin_transaction()` across `PostgresBackend`, `MysqlBackend`, and `SqliteBackend`.

- **v03 Pool m1 (Multi-statement execution failure in `MysqlBackend::execute`, `SqliteBackend::execute`, and `PostgresTransaction::execute` due to `sqlx::query` vs `sqlx::raw_sql`)** — **RESOLVED**.
  `PostgresBackend::execute` (`postgres.rs:138`), `MysqlBackend::execute` (`mysql.rs:147`), and `SqliteBackend::execute` (`sqlite.rs:163`) now consistently use `sqlx::raw_sql(&stmt).execute(&self.pool)`. In `PostgresTransaction`, `MysqlTransaction`, and `SqliteTransaction`, `DatabaseTransaction::execute` invokes `sqlx::Executor::execute(stmt.as_str())` on the transaction pointer, supporting multi-statement raw SQL scripts.

- **v03 Pool m2 (Inconsistent transaction auto-commit collision in `DatabasePool::transaction`)** — **RESOLVED**.
  In `PostgresTransaction::commit`, `MysqlTransaction::commit`, and `SqliteTransaction::commit` (`postgres.rs:239`, `mysql.rs:248`, `sqlite.rs:260`), `if let Some(tx) = guard.take()` safely ignores subsequent calls if the transaction was already resolved. If a closure commits the transaction explicitly, the outer `tx_handle.commit().await?` in `DatabasePool::transaction` (`pool.rs:179`) is an idempotent no-op returning `Ok(())`.

---

## Findings

### C1 Schema divergence in `Migrator::pending_migrations` and `verify_no_pending_backend` creates truncated `rume_migrations` table, breaking subsequent `run_pending` migrations

- **Where:** `rume/src/persistence/migration.rs:98–102, 251–255`, `rume/src/persistence/migration.rs:151–155, 185–195`, `rume/tests/containers/postgres.rs:107–120`
- **What it says:**
  In `rume/src/persistence/migration.rs`:
  ```rust
  pub async fn pending_migrations<B: DatabaseBackend>(
      &self,
      backend: &B,
  ) -> Result<Vec<String>, PersistenceError> {
      backend
          .execute(
              "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
          )
          .await?;
  ...
  pub async fn verify_no_pending_backend<B: DatabaseBackend>(
      &self,
      backend: &B,
  ) -> Result<(), PersistenceError> {
      backend
          .execute(
              "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, applied_at BIGINT NOT NULL)",
          )
          .await?;
  ```
  However, in `run_pending_backend` (`migration.rs:151–195`):
  ```rust
  backend
      .execute(
          "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, name VARCHAR(255) NOT NULL DEFAULT '', applied_at BIGINT NOT NULL, checksum VARCHAR(64) NOT NULL DEFAULT '')",
      )
      .await?;
  ...
  let insert_sql = format!(
      "INSERT INTO rume_migrations (id, name, applied_at, checksum) VALUES ('{}', '{}', {}, '{}')",
      migration.id.replace('\'', "''"),
      name,
      now,
      checksum
  );
  tx.execute(&insert_sql).await?;
  ```
- **Why it's a problem:**
  Standard application startup sequences and CI/CD deployment verification procedures run preflight checks (`migrator.verify_no_pending(&backend)` or `migrator.pending_migrations(&backend)`) before applying pending migrations.
  When either preflight method runs on a clean database, it executes the legacy DDL, creating `rume_migrations` with only two columns: `id` and `applied_at`.
  When `migrator.run_pending_backend(&backend)` is subsequently invoked, its `CREATE TABLE IF NOT EXISTS` statement is a no-op because the table already exists.
  When the runner attempts to record the applied migration via `INSERT INTO rume_migrations (id, name, applied_at, checksum) VALUES (...)`, the database engine aborts with a missing column error:
  - PostgreSQL: `column "name" of relation "rume_migrations" does not exist`
  - MySQL: `Unknown column 'name' in 'field list'`
  - SQLite: `table rume_migrations has no column named name`
  This defect is verified empirically by running `cargo test --test containers --features postgres -- postgres::test_postgres_migrations_up_and_rollback`:
  ```
  thread 'postgres::test_postgres_migrations_up_and_rollback' (888178) panicked at rume/tests/containers/postgres.rs:119:10:
  migrations should succeed: QueryFailed("error returned from database: column \"name\" of relation \"rume_migrations\" does not exist")
  ```
  This defect prevents clean-slate deployments from running preflight checks and migrations, breaking production boots.
- **Suggested fix:**
  Update lines 100 and 253 in `rume/src/persistence/migration.rs` to match lines 152 and 208:
  ```rust
  backend
      .execute(
          "CREATE TABLE IF NOT EXISTS rume_migrations (id VARCHAR(255) PRIMARY KEY, name VARCHAR(255) NOT NULL DEFAULT '', applied_at BIGINT NOT NULL, checksum VARCHAR(64) NOT NULL DEFAULT '')",
      )
      .await?;
  ```

---

### m1 Dead configuration property: `DatabaseConfig::auto_migrate` is never read or acted upon

- **Where:** `rume/src/persistence/config.rs:42, 53, 86–89`
- **What it says:**
  In `rume/src/persistence/config.rs`:
  ```rust
  pub struct DatabaseConfig {
      ...
      /// Whether to automatically execute pending migrations on application boot.
      #[serde(default)]
      pub auto_migrate: bool,
  }
  ...
  pub fn with_auto_migrate(mut self, auto: bool) -> Self {
      self.auto_migrate = auto;
      self
  }
  ```
- **Why it's a problem:**
  Callers configuring `DatabaseConfig::new(...).with_auto_migrate(true)` reasonably expect database initialization or application startup to automatically execute pending migrations. However, `self.auto_migrate` is never read or checked by any module, bootloader, or backend driver in Rume (`git grep -n "auto_migrate" rume/src/` matches only `config.rs`). The option creates a false sense of security that migrations are applied on startup.
- **Suggested fix:**
  Either connect `auto_migrate` to the module or application boot sequence (e.g. executing `migrator.run_pending()` during `Rume::start()` / `Module::on_start` when configured), or deprecate the field until bootloader migration support is implemented.

---

## Positive notes

- **Physical connection pool closure & bounded drain:** `DatabaseBackend::close()` and `DatabasePool::close_and_drain()` now cleanly coordinate with SQLx driver pools (`self.pool.close().await`), ensuring that connection sockets are terminated cleanly without leaked locks or sockets on shutdown.
- **Idempotent transaction commit and rollback:** `PostgresTransaction`, `MysqlTransaction`, and `SqliteTransaction` implement safe `guard.take()` semantics, allowing both manual commits inside `DatabasePool::transaction` closures and automatic completion without error collisions.
- **Robust health check deadline isolation:** `DatabaseHealthCheck::run()` and driver `ping()` implementations combine inner (3s) and outer (`config.connect_timeout()`) deadlines, guaranteeing that database saturation or partitioned networks will never stall Kubernetes liveness/readiness probes.
- **Transactional migration recording:** Enclosing DDL execution and migration table insertion within an atomic `DatabaseTransaction` ensures that failed migration scripts will not leave orphaned or inconsistent tracking records in `rume_migrations`.
- **Pool admission gating on outbox operations:** All outbox operations on `DatabasePool<B>` now acquire connection permits via `self.acquire_lease()`, preventing background relay sweeps from silently starving user queries.

---

## Verdict

**Fail** — this change cannot be accepted until a fix pass lands.

While the persistence and pool lifecycle improvements between `cf82559` and `HEAD` resolve 8 of the 9 prior findings (including both Critical findings C1 and C2), the remediation for migration table schema unification (v03 M3) was incomplete: `migration.rs:100` and `migration.rs:253` still execute obsolete 2-column DDL, causing preflight verification to break subsequent migration execution with SQL column errors and causing `test_postgres_migrations_up_and_rollback` in the container test suite to fail (Finding C1). Updating lines 100 and 253 to use the unified 4-column schema will unblock this subsystem for production acceptance.
