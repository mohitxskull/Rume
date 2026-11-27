# Task 02 — Multi-Dialect Job Queues, Priorities, Concurrency Caps & Job Batches

## Goal

Implement `SqliteJobQueue` and `MysqlJobQueue` alongside `PostgresJobQueue`, add named queue routing, numeric priority ordering, and per-queue concurrency caps in `JobWorker`, and implement `JobBatch` / `BatchStore` (`InMemoryBatchStore` and `SqlBatchStore`) with `then` / `finally` callbacks.

## Files touched

- `crates/data/sql/{sqlite,mysql,postgres}/jobs_*.sql` (verify/finalize dialect SQL)
- `crates/data/sql/{sqlite,mysql,postgres}/batches_*.sql`
- `crates/messaging/src/jobs/{mod.rs, job.rs, queue.rs, postgres.rs, sqlite.rs, mysql.rs, batch.rs}`
- `crates/messaging/src/jobs/worker/{mod.rs, runner.rs, tests.rs}`

## Deliverable

1. `SqliteJobQueue` (`sqlite` feature) and `MysqlJobQueue` (`mysql` feature) implementing `JobQueue` with `init_schema()`, atomic claim (`UPDATE ... RETURNING` on SQLite, transaction + `FOR UPDATE SKIP LOCKED` / `SELECT ... LIMIT 1` on MySQL), idempotency-key deduplication, lease renewal, DLQ, and purge.
2. Named queues (`queue` column, `Job::queue()`, `dequeue_from`) and numeric priorities (`priority` column, `Job::priority()`, `ORDER BY priority DESC, available_at ASC, created_at ASC`).
3. `JobWorker::with_queue_concurrency(queue, max_concurrent)` enforcing per-queue `Semaphore` limits alongside the global worker concurrency limit.
4. `JobBatch`, `BatchId`, `BatchStatus`, `BatchStore`, `InMemoryBatchStore`, `SqlBatchStore`, and `BatchBuilder` (`add_job`, `then`, `finally`, `dispatch`) tracking pending/completed/failed counts atomically and firing `then` when all jobs succeed or `finally` when the batch reaches any terminal state.

## Constraints honoured

- **SQLite/MySQL/Postgres parity**: `crates/messaging/src/jobs/postgres.rs` lease-fencing (`lease_token`), `defer` attempt-decrement, and `idempotency_key` deduplication semantics.
- **Worker drain safety**: `crates/messaging/src/jobs/worker/runner.rs` `admission_lock` and `is_stopping` drain invariants must remain intact when acquiring per-queue permits.
- **TDD (`rules/50-agent-conduct.md`, `rules/testing.md`)**: Write hermetic tests first against Task 01's `todo!()` stubs (confirming fails-before), then implement.

## Named tests (bodies omitted)

- `sqlite_job_queue_enqueues_dequeues_acks_and_dead_letters` — full lifecycle on in-memory SQLite.
- `job_queue_orders_by_priority_desc_then_available_at` — higher-priority jobs dequeue before lower-priority jobs.
- `job_queue_filters_by_named_queue` — `dequeue_from` only claims jobs from requested queues.
- `job_worker_enforces_per_queue_concurrency_cap` — verifies active count for a capped queue never exceeds its configured semaphore limit.
- `job_batch_fires_then_and_finally_on_all_success` — `then` and `finally` both execute once all jobs in the batch complete.
- `job_batch_skips_then_and_fires_finally_on_failure` — when a job dead-letters/fails, `then` is skipped and `finally` still executes.

## Verification

```bash
cargo test -j 2 -p rume-messaging --lib --all-features
cargo clippy -j 2 -p rume-messaging --all-targets --all-features -- -D warnings
```

## Commit

Only when the user asks:

```text
feat(messaging): add SQLite/MySQL job queues, priorities, queue caps, and job batches
```

## Done-when

- [ ] `SqliteJobQueue` and `MysqlJobQueue` implement the full `JobQueue` contract.
- [ ] Priority ordering, named queues, and per-queue `JobWorker` concurrency caps pass tests.
- [ ] `JobBatch` / `SqlBatchStore` / `InMemoryBatchStore` pass `then` and `finally` lifecycle tests.
- [ ] `cargo test -j 2 -p rume-messaging --lib --all-features` passes with zero warnings.
