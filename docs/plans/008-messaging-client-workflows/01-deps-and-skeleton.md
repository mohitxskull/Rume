# Task 01 — Dependencies, SQL Assets, Flake Fix, and Compiling Skeleton

## Goal

Pin all Milestone D dependencies in root `Cargo.toml`, wire Cargo features and DAG edges (`rume-enterprise` -> `rume-data`, `rume-http` `dns-srv`, `rume-messaging` `nats`/`rabbitmq`, `rume` facade), add SQL DDL/DML assets for SQLite/MySQL job queues, job batches, and SQL saga stores in `crates/data/sql/`, fix the `EventBus` tracing callsite-interest flake, and scaffold compiling `todo!()` skeletons across `rume-messaging`, `rume-http`, and `rume-enterprise`.

## Files touched

- `Cargo.toml`
- `crates/data/src/sql.rs`
- `crates/data/sql/sqlite/jobs_*.sql`, `crates/data/sql/mysql/jobs_*.sql`, `crates/data/sql/postgres/jobs_*.sql` (updated schema/insert/claim for `queue` + `priority`)
- `crates/data/sql/{sqlite,postgres,mysql}/batches_*.sql`
- `crates/data/sql/{sqlite,postgres,mysql}/saga_*.sql`
- `crates/messaging/Cargo.toml`
- `crates/messaging/src/lib.rs`
- `crates/messaging/src/events/bus.rs` (flake fix)
- `crates/messaging/src/events/distributed.rs` (skeleton)
- `crates/messaging/src/jobs/{mod.rs, job.rs, queue.rs, sqlite.rs, mysql.rs, batch.rs}` (skeletons + `queue`/`priority` fields)
- `crates/messaging/src/cron/{mod.rs}` (skeleton)
- `crates/messaging/src/pubsub/{mod.rs, memory.rs, redis.rs, postgres.rs}` (skeletons)
- `crates/messaging/src/broker/{mod.rs, memory.rs, nats.rs, rabbitmq.rs}` (skeletons)
- `crates/http/Cargo.toml`
- `crates/http/src/lib.rs`, `crates/http/src/client/{mod.rs, discovery.rs}` (skeletons)
- `crates/enterprise/Cargo.toml`
- `crates/enterprise/src/saga/coordinator/{mod.rs, sql_store.rs, redis_store.rs}` (skeletons)
- `crates/core/Cargo.toml`

## Deliverable

1. Root `Cargo.toml` pins `croner = "=5.0.1"`, `hickory-resolver = "=0.26.4"`, `async-nats = "=0.53.0"`, and `lapin = "=4.13.0"`, and `cargo deny check licenses` passes.
2. `rume-data::sql` exports SQL constants for SQLite/MySQL/Postgres job queues (including `queue` and `priority` columns), job batches (`rume_job_batches`), and durable saga stores (`rume_sagas`).
3. `crates/messaging/src/events/bus.rs` fixes `event_bus_publish_sync_creates_span_and_propagates_trace_context` by serializing subscriber-override tests with a `std::sync::Mutex` and calling `tracing::callsite::rebuild_interest_cache()` before creating test spans.
4. All new modules and public trait/struct signatures compile cleanly with `todo!()` method bodies under `--all-features` and default features.

## Constraints honoured

- **Concrete pins & licenses**: `rules/10-tech-stack.md`, `deny.toml` (`MIT`, `Apache-2.0`, `MIT OR Apache-2.0`).
- **7-layer DAG**: `rume-enterprise` (L3) -> `rume-data` (L2); no upward or cyclic crate edges.
- **SQL centralization**: `crates/data/src/sql.rs` owns all SQL `include_str!` assets.
- **Backward compatibility**: `Job::queue()` defaults to `"default"`, `Job::priority()` defaults to `0`, and `JobQueue::dequeue_from` defaults to `self.dequeue_for(worker_id)`.
- **`#![forbid(unsafe_code)]`**: Preserved in all library crates.

## Named tests (bodies omitted)

- `rume_messaging::events::bus::tests::event_bus_publish_sync_creates_span_and_propagates_trace_context` — passes deterministically across repeated parallel runs (`--test-threads=8`).

## Verification

```bash
cargo deny check licenses
cargo check -j 2 --workspace --all-targets --all-features
cargo test -j 2 -p rume-messaging --lib event_bus_publish_sync -- --test-threads=8
```

## Commit

Only when the user asks:

```text
chore(plan-008): pin milestone D dependencies and scaffold compiling skeletons
```

## Done-when

- [ ] `cargo deny check licenses` passes with zero errors or license warnings.
- [ ] `cargo check -j 2 --workspace --all-targets --all-features` exits 0.
- [ ] `cargo check -j 2 -p rume` (default features) exits 0.
- [ ] `event_bus_publish_sync_creates_span_and_propagates_trace_context` passes reliably.
