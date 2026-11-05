# Persistence & Pool Lifecycle Review — v04

## Summary

The v03/v03-diff remediations for this lens landed and are real: `DatabaseBackend::close()` is implemented per driver and awaited under a bounded timeout in `close_and_drain()`; pool sizing (`max/min/acquire_timeout/idle_timeout`) is forwarded to all three SQLx pool implementations; the `rume_migrations` DDL is unified across all four creation paths and the PostgreSQL container migration test passes; DB-clock authority is complete and consistent across Postgres (`clock_timestamp()`), MySQL (`UNIX_TIMESTAMP()`) and SQLite (`strftime('%s','now')`) with no `now()` anywhere in the lease paths. What remains is one honest gap repeated from v03 and never actually delivered (the HA/distributed migration lock — MySQL DDL is non-transactional by definition, and no `pg_advisory_lock`/`GET_LOCK` exists anywhere), a stored-but-never-verified migration checksum, and a set of polish defects (dead remediation scaffolding, shadow-only telemetry, host-clock ledger). No Critical findings: the shutdown path no longer reports false success.

---

## Prior decisions

Classifications use this review's vocabulary (REAFFIRMED / CHALLENGED / NOTE-NEW). Evidence is static (file:line) or executed (command + result) as marked; per-finding executed/not-executed status is also recorded in the verification log at the end of the Findings section.

### From v02

- **v02 Correctness M3 (`Migrator::rollback_last` silently unmarks migrations when `down_sql` is `None`)** — **REAFFIRMED**. `crates/core/src/persistence/migration.rs:219–223` fails closed with `PersistenceError::IrreversibleMigration(id)` before any tracking write; the `rume_migrations` row is preserved. Executed: `cargo test -p rume --features sqlite --test integration -- persistence::` → `irreversible_migration_rollback_fails_closed ... ok`; container `test_postgres_migrations_up_and_rollback` → `ok`.
- **v02 Enterprise-Reliability C2 / Group B (`TransactionalOutbox::save_tx` transactional participation)** — **REAFFIRMED**. All three backends implement `TransactionalOutbox<*Transaction>` (`postgres.rs:607`, `mysql.rs:623`, `sqlite.rs:632`) by executing inside the caller's transaction; `DatabasePool<B>` delegates without acquiring a lease, which is correct — the lease would double-gate work already inside a tx (`pool.rs:522–533`). Executed: `cargo test -p rume --features postgres --test containers -- postgres::test_postgres_transactional_outbox_atomicity` → `1 passed`.
- **v02 positive note (transaction handles roll back on drop)** — **REAFFIRMED**. Executed: `cargo test -p rume --features mysql --test containers -- mysql::test_mysql_transaction_drop_auto_rollback` → `ok`. Handles wrap `sqlx::Transaction` in `Arc<Mutex<Option<…>>>` (`postgres.rs:195`, `mysql.rs:204`, `sqlite.rs:220`); drop without commit lets SQLx roll back.
- **v03/diff v01 remediation "Testing M3" (closed-pool queries must fail, not just boolean flags)** — **REAFFIRMED**. `crates/core/tests/containers/postgres.rs:95–100` asserts a post-close `pool.backend().execute("SELECT 1")` fails with `PersistenceError::Closed`; executed (`postgres::test_postgres_pool_lifecycle_and_health ... ok`).

### From v03 / v03-diff (own-lens findings)

- **v03 C1 (`close()`/`close_and_drain()` never closed physical pools; false shutdown success)** — **REAFFIRMED**. `DatabaseBackend` now declares `close()`/`is_closed()`/`pool_size()`/`pool_idle()` (`backend.rs:30–47`); all three backends implement `close()` as `self.pool.close().await` (`postgres.rs:181–185`, `mysql.rs:190–194`, `sqlite.rs:206–210`). `close_and_drain()` sets `is_closed`, refuses new leases, drains lease-tracked work, then awaits `backend.close()` inside the remaining timeout budget (`pool.rs:220–239`). SQLx 0.8.6's `Pool::close()` closes idle connections and then waits for all in-use connections to be returned before resolving (`sqlx-core-0.8.6/src/pool/inner.rs:97–115`, verified in vendored source), so in-flight transactions and backend-direct queries genuinely block closure — the residual drag is honestly reported by returning `false`. Executed: container `postgres::test_postgres_pool_lifecycle_and_health` → `ok` (queries after close → `PersistenceError::Closed`); `sqlite --test integration -- persistence::pool_acquire_timeout_and_graceful_drain` → `ok`. Note: `close()` itself remains fire-and-forget — see **m5**.
- **v03 C2 (disconnected "potemkin" pool: fictional metrics, shadow semaphore)** — **CHALLENGED**. The critical half is fixed and verified: `PoolStatus.total_connections` and `is_closed` now derive from the driver (`pool.rs:110–111`), outbox operations acquire permits through `acquire_lease()` (`pool.rs:355–520`), and physical closure is driver-authoritative. But the remediation stops halfway: `active_connections` is still the shadow counter (`pool.rs:112–114`, health meta at `pool.rs:300–302`), `pool_idle()` is implemented on all backends yet never surfaced, the `total_connections` atomic is dead (written at `pool.rs:35,73`, read nowhere), and the shadow semaphore still gates lease-tracked work independently of the driver pool. This is the residual half of the same finding, not a new class of defect — filed as **m1** (polish-grade now that shutdown correctness no longer depends on it).
- **v03 M1 (pool config silently ignored)** — **REAFFIRMED**. All three backends forward `max_connections`/`min_connections`/`acquire_timeout`/`idle_timeout` (`postgres.rs:44–54, 61–67`; `mysql.rs:50–60, 67–73`; `sqlite.rs:82–89`); `SqliteBackend::open()` routes through `DatabaseConfig` instead of the old hardcoded `max_connections(10)` (`sqlite.rs:59–97`); in-memory SQLite pins `max_connections(1)` (`sqlite.rs:49–53`), which is required for correctness of `sqlite::memory:` semantics.
- **v03 M2 (health check / ping hang under saturation)** — **REAFFIRMED** (code-verified; not executed under live saturation). Driver `ping()` runs inside a 3 s `tokio::time::timeout` returning `PersistenceError::Timeout` (`postgres.rs:119–131`, `mysql.rs:128–140`, `sqlite.rs:144–156`); `DatabaseHealthCheck::run()` additionally bounds the ping by `config.connect_timeout()` (`pool.rs:322–346`). Stale TCP connections are additionally caught at checkout by SQLx's default `test_before_acquire(true)` (verified in `sqlx-core-0.8.6/src/pool/options.rs:149,256`).
- **v03 M3 (`rume_migrations` NOT NULL divergence; `run_pending` crashes)** — **REAFFIRMED**. One unified 4-column DDL is used at all four sites (`migration.rs:100, 152, 208, 253`); `name`/`checksum` are populated from description and SHA-256 of `up_sql` (`migration.rs:177–184`); schema files agree (`sql/*/migrations_schema.sql`). Executed: container `postgres::test_postgres_migrations_up_and_rollback` → `ok`; SQLite `persistence::migrations_apply_rollback_and_preflight_verification` → `ok`. The four drifting copies remain — see **m3**.
- **v03 M4 (non-atomic migration application; no distributed lock in HA)** — **CHALLENGED**. Half delivered: DDL + tracking INSERT/DELETE now run inside one transaction (`migration.rs:164–193`, `225–238`), which is genuinely atomic on PostgreSQL and SQLite (transactional DDL). The HA half was never implemented: no advisory lock exists anywhere (`grep -ri 'pg_advisory\|GET_LOCK'` over `rume/` → 0 hits), and on MySQL the "transaction" is decorative — InnoDB DDL causes an implicit commit before and after execution and cannot be rolled back (MySQL 8.0 Reference Manual §15.3.3, Statements That Cause an Implicit Commit; "The `CREATE TABLE` statement in InnoDB is processed as a single transaction"). The v03 P1 `todo.md` item promised "Acquire a distributed lock / advisory lock before applying migrations" and is marked `[x]`; the code does not do it. Filed as **M1**.
- **v03 M5 (SQLx error flattening to `QueryFailed`/`TransactionError`)** — **REAFFIRMED** for its stated scope. `map_sqlx_error` (`backend.rs:55–63`) maps `PoolTimedOut→Timeout`, `PoolClosed→Closed`, `Io→ConnectionFailed` and is used by `ping()`, `execute()`, `query_scalar()`, `begin_transaction()` in all three backends. The transaction-scoped paths were never mapped — see **M3** (NOTE-NEW).
- **v03 m1 (multi-statement execution via `sqlx::query`)** — **REAFFIRMED**. Pool-wide `execute()` uses `sqlx::raw_sql` (`postgres.rs:136`, `mysql.rs:145`, `sqlite.rs:161`); transaction `execute()` uses `Executor::execute(&str)`, which `sqlx` implements with `take_arguments() == Ok(None)` — the simple query protocol (verified in `sqlx-core-0.8.6/src/executor.rs:215–234`), and MySQL connections enable `CLIENT_MULTI_STATEMENTS` (verified in `sqlx-mysql-0.8.6/src/protocol/capabilities.rs:53` and `.../protocol/connect/handshake.rs:117,173`). Multi-statement scripts work on all three dialects.
- **v03 m2 (auto-commit collision in `DatabasePool::transaction`)** — **REAFFIRMED**. `commit()`/`rollback()` take the inner transaction out of the `Mutex<Option<_>>`; repeated calls are idempotent no-ops (`postgres.rs:242–248, 253–261`; `mysql.rs:251–257, 263–270`; `sqlite.rs:266–273, 278–285`). A closure that commits explicitly no longer produces a spurious outer error.
- **v03/diff v01 C1 (preflight DDL creates truncated 2-column `rume_migrations`; container test fails)** — **REAFFIRMED**. `pending_migrations()` (`migration.rs:94–116`), `verify_no_pending_backend()` (`migration.rs:247–274`), `run_pending_backend()` (`migration.rs:146–199`) and `rollback_last_backend()` (`migration.rs:202–244`) all create the identical 4-column table; the container test that failed in v03 passes today. Executed: `cargo test -p rume --features postgres --test containers -- postgres::test_postgres_migrations_up_and_rollback` → `1 passed; 0 failed`.
- **v03/diff v01 m1 (`DatabaseConfig::auto_migrate` never read)** — **CHALLENGED**. The remediation added `DatabasePool::auto_migrate()` and `apply_auto_migrations()` (`pool.rs:85–101`), but **nothing calls them** — `grep -rn 'apply_auto_migrations|auto_migrate('` over `rume/src` and `rume/tests` matches only the definitions in `pool.rs`/`config.rs`. The option still does nothing by default; no boot path, module preflight, CLI or test ever invokes it. Scaffolding, not wiring — filed as **m2**.
- **Outbox lease operations through `DatabasePool` (v03 todo P2 "Outbox m4")** — **REAFFIRMED**. Every `OutboxRepository`/`TransactionalOutbox` method on `DatabasePool<B>` acquires a lease before delegating (`pool.rs:351–520`), including `mark_failed_with_max_retries` and `renew_outbox_lease`.

**No area of this lens lacked prior coverage entirely** — prior cycles covered the whole subject but shallowly: the checksum-never-verified gap (M2) and the host-clock ledger (m4) were never identified by any prior review, and are NOTE-NEW.

---

## Findings

### M1 — HA migration path is unguarded: no advisory/distributed lock, and MySQL DDL defeats the migration transaction

- **Where:** `crates/core/src/persistence/migration.rs:164–193` (`run_pending_backend`), `crates/core/src/persistence/migration.rs:225–238` (`rollback_last_backend`), `crates/core/src/persistence/mysql.rs:165–176` (`begin_transaction`); absence verified by `grep -ri 'pg_advisory\|GET_LOCK' rume/` → 0 hits.
- **What it says:** Each pending migration runs its DDL and its tracking INSERT inside `backend.begin_transaction()`, with no cross-process lock of any kind. On PostgreSQL, InnoDB-style DDL is transactional, so a *failed* migration rolls back cleanly — but two replicas booting concurrently both observe the migration as pending and both execute the same DDL: the loser blocks on the DDL lock and then fails with `relation/column ... already exists`, crash-looping that replica until the winner's tracking row lands. On MySQL the situation is worse: `CREATE TABLE`/`ALTER TABLE` cause an implicit commit before and after execution and cannot be rolled back (MySQL 8.0 RM §15.3.3), so the `tx.execute(ddl)` → error → drop-`Transaction` → `ROLLBACK` path undoes **nothing** — already-executed DDL persists, the tracking row is never written, and the next boot re-runs the migration and fails on the existing table.
- **Why it's a problem:** The v03 todo promised a distributed/advisory lock before applying migrations and is marked `[x]`; it was never implemented. For a workload whose entire purpose is "run migrations unattended at deploy time across replicas", the failure mode is a wedged deployment requiring manual DBA intervention — and on MySQL, silent partial-DDL application with a lying bookkeeping state. PostgreSQL/SQLite are sound on the failure path; the HA race affects all three.
- **Suggested fix:** Acquire a backend-appropriate advisory/distributed lock around the whole `run_pending`/`rollback_last` section and hold it for its duration (e.g. PostgreSQL `SELECT pg_advisory_lock(<constant>)` / `pg_advisory_unlock`; MySQL `SELECT GET_LOCK('rume_migrations', 30)` / `RELEASE_LOCK`; SQLite: `BEGIN IMMEDIATE` or a single-row lock table). On lock timeout, fail closed with a distinct error. Additionally, for MySQL treat DDL failure as **non-recoverable without operator action**: after a failed migration, verify on next boot that already-applied DDL matches expectations before re-executing (or at minimum surface a `PersistenceError::MigrationError` naming the migration and the implicit-commit hazard). Add a container test with two concurrent `run_pending_backend` calls asserting at-most-once DDL execution.

### M2 — Migration checksums are computed and stored but never verified

- **Where:** `crates/core/src/persistence/migration.rs:178–184` (checksum computed from `up_sql`), `:186–191` (INSERT stores it); absence verified by `grep -rn checksum rume/` → the only occurrences are the schema files, the four DDL copies and the write path — no read/comparison exists anywhere. `verify_no_pending_backend` (`:247–274`) checks only row presence by `id`.
- **What it says:** Every applied migration records `checksum = SHA-256(up_sql)` in `rume_migrations.checksum`. On every subsequent boot, verification re-checks migration **identity** (does a row with this `id` exist?) but never re-verifies **content** — a migration file edited after being applied (wrong statement, widened column, dropped guard) is silently trusted forever.
- **Why it's a problem:** The checksum column exists to detect exactly this class of drift — between staging and production, between the release artifact and what was applied, between an edited in-tree migration and what ran last quarter. As shipped, it is write-only data providing zero integrity. This is the silent-divergence class that migration systems exist to prevent, and the failure surfaces only as an unexplainable production schema.
- **Suggested fix:** In `pending_migrations` / `verify_no_pending_backend` / `run_pending_backend`, for every already-applied migration recompute `SHA-256(up_sql)` and compare with the stored `checksum`; on mismatch fail closed with a structured error (e.g. `PersistenceError::MigrationError { migration, message: "checksum mismatch: file changed after application" }`, or a dedicated `ChecksumMismatch` variant). Migrations with `up_sql == None` have no checksum — treat as exempt. Add a unit test: apply migration, tamper with the registered up_sql string, verify fails closed.
- *NOTE-NEW: no prior review identified this.*

### M3 — Transaction handles flatten SQLx errors; `ConnectionFailed`/`Closed`/`Timeout` are unreachable mid-transaction

- **Where:** `crates/core/src/persistence/postgres.rs:217, 233, 245, 258`; `crates/core/src/persistence/mysql.rs:226, 242, 254, 267`; `crates/core/src/persistence/sqlite.rs:242, 258, 270, 283`.
- **What it says:** Inside `*Transaction::execute`/`query_scalar`, every SQLx error is mapped to `QueryFailed(e.to_string())`; inside `commit`/`rollback` every error is mapped to `TransactionError(e.to_string())`. The backend-level paths deliberately use `map_sqlx_error` (`backend.rs:55–63`), which produces `Timeout`, `Closed`, `ConnectionFailed`, `QueryFailed`; the transaction paths bypass it entirely. An I/O error mid-transaction (dropped TCP connection, server restart) arrives to callers as `QueryFailed("…")` — indistinguishable from a constraint violation.
- **Why it's a problem:** The persistence charter for this review requires "no flattened strings" **across transactions**, and the crate's whole error taxonomy exists so resilience middleware (retry, circuit breaker) can classify by variant. A connection reset during a migration-critical transaction is reported as a plain query failure, defeating variant-based retry/route/circuit decisions and misreporting the failure to logs and alerts. The `map_sqlx_error` helper already exists and is used correctly one call frame up — the omission is an oversight, not a decision (no rationale comments exist at these twelve sites).
- **Suggested fix:** Use `map_sqlx_error` in the nine `execute`/`query_scalar` sites. For `commit`/`rollback`, add `map_sqlx_tx_error` alongside it: `Io→ConnectionFailed`, `PoolClosed→Closed`, `PoolTimedOut→Timeout`, `Database(e)→TransactionError(e.to_string())` (preserving transactional context for serialization/constraint failures), else `TransactionError(e.to_string())`. Add unit tests asserting an injected `sqlx::Error::Io` maps to `ConnectionFailed` on a transaction handle.
- *NOTE-NEW: v03 M5 covered backend-level paths; the transaction-scoped gap was never covered.*

### m1 — Pool telemetry is still half shadow: dead `total_connections` atomic, lease-only `active_connections`, driver idle never surfaced

- **Where:** `crates/core/src/persistence/pool.rs:34–38` (fields), `:66–78` (init), `:109–120` (`status()`), `:300–310` (health-check meta), `:23–27` (`PoolStatus`); `crates/core/src/persistence/backend.rs:34–37` and per-backend `pool_idle()` (`postgres.rs:173–175`, `mysql.rs:182–184`, `sqlite.rs:198–200`).
- **What it says:** `PoolStatus::total_connections` is driver-derived, but `active_connections` counts only lease-tracked operations — a long query issued through `pool.backend()` (e.g. by `Migrator` or user code) is invisible to it, so health meta reports `0` active while the physical pool is saturated. `PoolStatus` has no `idle_connections` field although every backend implements `pool_idle()`. The `total_connections` atomic (`pool.rs:35,73`) is initialized and cloned but never read.
- **Why it's a problem:** Operators reading `active_connections`/`total_connections` get a mixture of one real and one fictitious number; capacity alerting built on it misfires in exactly the scenarios (saturation via direct backend use) that justify a pool. Charter item 1 requires size/idle telemetry reflected from the driver.
- **Suggested fix:** Delete the dead `total_connections` atomic; add `idle_connections` to `PoolStatus` from `backend.pool_idle()`; rename the shadow counter to `leased_connections` (or derive `active = total − idle`, clamped at 0) so its semantics are explicit; update health-check meta keys. If the shadow counter is kept purely as admission accounting, say so in a comment at `pool.rs:34–38`.

### m2 — `auto_migrate` is inert: `apply_auto_migrations()` has zero callers

- **Where:** `crates/core/src/persistence/pool.rs:85–101`; `crates/core/src/persistence/config.rs:40–42, 86–89`. Verified: `grep -rn 'apply_auto_migrations\|auto_migrate(' rume/src rume/tests` → only the definitions.
- **What it says:** `DatabaseConfig::with_auto_migrate(true)` is accepted and stored, and `DatabasePool` offers a method that "automatically applies pending migrations on the backend if `auto_migrate` is configured" — but no boot path, module preflight, CLI command, or test ever calls it.
- **Why it's a problem:** The option advertises automatic schema management that never happens; a deploy relying on it boots against an unmigrated database and fails later with obscure errors. This is the v03/diff m1 remediation completed in shape but not in effect.
- **Suggested fix:** Either wire it — invoke `pool.apply_auto_migrations(&migrator)` in the module/app preflight behind the flag (wiring belongs to the lifecycle layer; coordinate with the app-lifecycle lens — `Module::on_start` is the natural site) — or remove `auto_migrate`/`with_auto_migrate`/`apply_auto_migrations` until that wiring exists. Pick one; half-measures already failed once.

### m3 — Four drifting copies of the `rume_migrations` DDL; drift already present

- **Where:** `crates/core/src/persistence/migration.rs:100, 152, 208, 253` (inline copies, all identical) vs `sql/postgres/migrations_schema.sql`, `sql/mysql/migrations_schema.sql`, `sql/sqlite/migrations_schema.sql`; `PostgresBackend::init_migrations_table()` etc. execute the files. Divergence: MySQL file declares `applied_at BIGINT UNSIGNED NOT NULL` and no-engine parity with `ENGINE=InnoDB`; SQLite file declares `id TEXT`; the inline copy declares `BIGINT NOT NULL` / `VARCHAR(255)` (works, but not identical).
- **What it says:** The table DDL lives in four .sql files plus four inline Rust string copies, all claiming to be the same table. Column sets currently agree (after the v03/diff C1 fix), but nothing prevents re-drift — and re-drift is precisely what produced the v03 and v03/diff C-grade failures on this exact table.
- **Why it's a problem:** The two DDL families have already diverged twice; the remediation fixed the symptom, not the root cause. Every future change to `rume_migrations` must be made identically in eight places or the failure recurs.
- **Suggested fix:** Make the SQL files the single source (`include_str!("../sql/<dialect>/migrations_schema.sql")` selected on `dialect()`), or generate the inline constants from them at compile time; in both cases add a unit test asserting that the executed DDL and the file DDL produce the same `information_schema` column set per dialect. Delete the four inline literals.

### m4 — Migration ledger `applied_at` uses the host clock in a DB-clock-authoritative subsystem

- **Where:** `crates/core/src/persistence/migration.rs:173–176` (`SystemTime::now()` → `INSERT ... applied_at`).
- **What it says:** The lease lifecycle was moved to database wall-clock authority in v03/v04 (`a59bab0`, `11bbe3a`, `bd397cd`) because host clocks skew; the migration ledger still records application time from the host.
- **Why it's a problem:** `applied_at` is the audit answer to "when did production apply migration N?" — under clock skew it yields misleading (even backwards) ledger entries, and it is the one timestamp in the subsystem not governed by the same rule the rest of the codebase was just hardened to follow.
- **Suggested fix:** Fetch the DB wall clock (`query_scalar` per dialect: `FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT` / `UNIX_TIMESTAMP()` / `CAST(strftime('%s','now') AS INTEGER)`) or insert `NULL`-able `GREATEST(...)`-style expressions inline; alternatively document `applied_at` as informational-only in the struct doc. Prefer the query — one scalar round-trip at migrate time is free.

### m5 — `DatabasePool::close()` is fire-and-forget; no in-tree shutdown path closes any pool

- **Where:** `crates/core/src/persistence/pool.rs:207–215`; absence of callers verified by `grep -rn 'close_and_drain' rume/src` → 0 hits (tests only: `crates/core/tests/integration/persistence.rs:57,207,254`, `crates/core/tests/containers/postgres.rs:83`, `crates/core/tests/containers/pipeline.rs:318,787`; `close()` callers are tests and one test module hook).
- **What it says:** `close()` sets `is_closed`, closes the semaphore, and `tokio::spawn`s `backend.close()` — the caller cannot await it, the spawn is unobservable, and if the runtime begins teardown the physical pools may never be closed. `close_and_drain(timeout)` exists, works (executed tests), and is symmetric — but nothing in `src/` ever calls it; application authors must discover it themselves.
- **Why it's a problem:** A public `close()` that returns `()` while doing its real work in a detached task is an API footgun: shutdown ordered by the application (stop accept → drain → close pool) can report success while connections are still open. The framework owning no shutdown callsite compounds it — pool closure is entirely caller-discipline.
- **Suggested fix:** Make `close()` async (delegating to `close_and_drain(Duration::from_secs(...))` or awaiting `backend.close()`), or return the `JoinHandle`, or deprecate it in favour of `close_and_drain` with the timeout chosen by the caller. Whichever is chosen, the lifecycle layer (`src/app/`) must invoke it on shutdown — that wiring is the app-lifecycle lens's to verify; this finding flags the persistence-side API.

### m6 — Dead scaffolding: `MigrationFn` defined, never used

- **Where:** `crates/core/src/persistence/migration.rs:9–13`; verified by `grep -rn 'MigrationFn' rume/` → only the definition. (Similarly `init_migrations_table()` at `postgres.rs:94`, `mysql.rs:100`, `sqlite.rs:116` has no in-tree caller, but it is deliberate public API for external schema tooling and is retained.)
- **What it says:** The type alias for dynamic migration functions exists in the public API; the runner never accepts one, and no code path constructs one.
- **Why it's a problem:** Dead public API implies a capability that does not exist — a user reading the signature will try to pass dynamic migrations and fail. It is also the third instance this review found of remediation-adding-scaffolding-without-callers (see m2), suggesting a pattern worth a ruling in the fix pass.
- **Suggested fix:** Remove `MigrationFn` (or implement dynamic migration functions in the runner if that is the documented intent — it is not). Record the ruling in the todo group.

**Shared root cause:** m1 (dead atomic), m2, m6 are the same missing rule — *a remediation is not complete until a caller invokes it and a test exercises it*. m3 is the missing rule behind both v03 C1-class defects — *one fact, one representation*. M1 is the undelivered half of v03 M4; M3 is the undelivered half of v03 M5. The conductor's `todo.md` should group m1/m2/m6 together, and M1/M2/M3 as migration-integrity + error-taxonomy completion.

---

### Verification log

**Static verification** (read in full): `crates/core/src/persistence/{mod,backend,config,error,migration,pool,postgres,mysql,sqlite}.rs`, all `sql/*/migrations_schema.sql`, `sql/*/outbox_{claim,recover,insert}.sql`, `crates/core/tests/integration/persistence.rs`, `crates/core/tests/containers/{postgres,mysql,pipeline}.rs` (pool-relevant excerpts), SQLx 0.8.6 vendored source (`pool/inner.rs`, `pool/options.rs`, `executor.rs`, `sqlx-mysql/src/protocol/*`).

**Executed verification** (Docker available; all green):

```
cargo test -p rume --features postgres --test containers -- \
    postgres::test_postgres_migrations_up_and_rollback \
    postgres::test_postgres_pool_lifecycle_and_health \
    postgres::test_postgres_pool_connection_limits_and_timeout \
    --exact --test-threads=1
  → 3 passed; 0 failed

cargo test -p rume --features postgres --test containers -- \
    postgres::test_postgres_transactional_outbox_atomicity --exact --test-threads=1
  → 1 passed; 0 failed

cargo test -p rume --features mysql --test containers -- \
    mysql::test_mysql_pool_lifecycle_and_ping --exact --test-threads=1
  → 1 passed; 0 failed

cargo test -p rume --features mysql --test containers -- \
    mysql::test_mysql_transaction_drop_auto_rollback --exact --test-threads=1
  → 1 passed; 0 failed

cargo test -p rume --features sqlite --test integration -- persistence:: --test-threads=1
  → 7 passed; 0 failed  (pool_lifecycle_and_capacity_limits, explicit_transaction_commit_and_rollback,
     migrations_apply_rollback_and_preflight_verification, irreversible_migration_rollback_fails_closed,
     health_check_reports_pool_status_and_failures, pool_acquire_timeout_and_graceful_drain,
     module_persistence_end_to_end)
```

DB-clock authority (mandatory check): claim, renewal, recovery, and completion all use one semantic per backend — PostgreSQL `clock_timestamp()` (`sql/postgres/outbox_claim.sql:5`, `outbox_recover.sql:6`, `postgres.rs:364,399,425,456,474`), MySQL `UNIX_TIMESTAMP()` (`sql/mysql/outbox_claim.sql:5`, `outbox_recover.sql:6`, `mysql.rs:381,415,441,472,490`), SQLite `strftime('%s','now')` (`sqlite.rs:389,424,450,481,499`). `now()` does not appear in any persistence path. Semantics are consistent within each backend and across all three.

**Not executed:** (i) health check under live pool saturation against Postgres/MySQL — no test does this; M2-v03's fix is code-verified only. (ii) MySQL migration suite — no migration container test exists (only Postgres has one); the MySQL/SQLite parity claim for migrations rests on identical code paths, not on runs. (iii) Concurrent `run_pending_backend` from two processes — the M1 failure mode is untested, by definition of its absence. (iv) Feature-matrix builds beyond the features above — not needed for this lens; covered by the testing lens.

---

## Positive notes

- **Physical closure is real and bounded.** `close_and_drain()` refuses new leases, awaits lease-tracked work, then drives driver closure under the remaining budget; SQLx itself waits for in-use connections, so in-flight transactions hold termination — verified in the vendored SQLx source and by the executed container/integration tests.
- **Migration DDL unification actually landed.** One 4-column `rume_migrations` everywhere, SHA-256 checksum recorded, and rollback fails closed on irreversible migrations (executed).
- **Database-clock authority is complete.** Every claim/renew/recover/completion path is server-side epoch in all three dialects — the work from `a59bab0`/`11bbe3a`/`bd397cd` is verified present, not claimed.
- **Transactions are idempotent and drop-safe.** `guard.take()` makes repeat `commit()`/`rollback()` no-ops; dropped handles roll back (executed on MySQL); multi-statement DDL works on all three dialects via the simple query protocol — a non-obvious SQLx property that is now relied upon and tested.
- **MySQL claim isolation is right.** `SET TRANSACTION ISOLATION LEVEL READ COMMITTED` before `BEGIN` (`mysql.rs:361–364`) defeats REPEATABLE-READ phantom protection masking newly-arrived outbox rows — preserved and correct.
- **SQLite embedded defaults are correct.** `:memory:` pinned to one connection (correct `sqlite::memory:` semantics), WAL journal mode, `busy_timeout(5s)`, `foreign_keys(true)` (`sqlite.rs:43–97`).
- **Config forwarding is complete and honest.** `min/max/acquire/idle` on all three backends; file-backed SQLite routes through `DatabaseConfig`; `DatabasePool::apply_auto_migrations` at least makes the intent executable — even if unowned (m2).
- **`#![forbid(unsafe_code)]`** intact (`lib.rs:8`); **MariaDB aliases** are transparent type aliases, not reimplemented backends; SQL is externalized via `include_str!` rather than string-spliced.

---

## Verdict

**Ready to proceed; not ready to ship unattended production migrations without M1–M3.**

No Critical findings: the shutdown path no longer reports false success, physical connections and in-flight transactions terminate under a bounded, honestly-reported drain, and the migration preflight/apply cycle passes on live PostgreSQL and SQLite. The subsystem is fit to keep building on, provided three Majors land before the next HA/multi-replica deployment that runs migrations: **M1** (advisory/distributed migration lock; MySQL DDL implicitly commits and cannot roll back — the current `begin_transaction` wrapper does not make migration application atomic on MySQL), **M2** (verify the stored migration checksum or drop the column — write-only checksums are false assurance), and **M3** (transaction handles must not flatten `ConnectionFailed`/`Closed`/`Timeout` into `QueryFailed`/`TransactionError`). Minor items (m1–m6) are polish and should be folded into the same fix pass rather than deferred, since three of them are dead remediation scaffolding from this very cycle. If M1–M3 are deferred, every subsequent release inherits a deployment-time failure mode that fails loud, wedges migrations, and demands manual DBA repair.
