# Plan 008 — Milestone D: Distributed Messaging, Outbound Client & Workflows

## Goal

Close all six Milestone D distributed messaging, outbound HTTP client, and workflow orchestration gaps cleanly and idiomatically, executed **strictly sequentially on `main`** (one task at a time):

1. **Multi-Dialect & Advanced Job Queues (`rume-messaging` + `rume-data::sql`)** — Implement `SqliteJobQueue` and `MysqlJobQueue` alongside `PostgresJobQueue` using central `rume_data::sql` constants; add named queues (`queue`), numeric job priorities (`priority` ordering), per-queue concurrency caps in `JobWorker`, and **Job Batches / Workflows** (`JobBatch`, `BatchStore`, `InMemoryBatchStore`, `SqlBatchStore` with `then` / `finally` callbacks). Also fix the pre-existing `rume-messaging::events::bus::tests::event_bus_publish_sync_creates_span_and_propagates_trace_context` `tracing-core` callsite-interest flake.
2. **Leader-Elected Cron Scheduler (`rume-messaging`)** — Implement `CronScheduler` combining `rume_data::coordination::LeaderElector` + `croner = "=5.0.1"` + `JobQueue` / async handlers so recurring cron jobs execute once across a multi-instance cluster, with UTC/fixed-offset timezone support, `MissedTickPolicy`, and graceful shutdown.
3. **Instrumented Outbound HTTP Client (`RumeClient` in `rume-http`)** — Implement `RumeClient` / `RumeClientBuilder` wrapping `reqwest = "=0.13.5"` with automatic W3C `traceparent`/`tracestate` and `X-Request-ID` injection (`rume-observability`), `CircuitBreaker` (`TrialPermit`), configurable exponential backoff + jitter retries (idempotent-by-default), per-attempt & total timeouts, and `ServiceDiscovery` + feature-gated `DnsSrvDiscovery` (`hickory-resolver = "=0.26.4"` behind `dns-srv`).
4. **Distributed `PubSub` & `EventBus` (`rume-messaging`)** — Implement `PubSub` trait with `InMemoryPubSub`, `RedisPubSub` (`redis` feature), and `PostgresPubSub` (`postgres` feature via `sqlx::postgres::PgListener`), plus `DistributedEventBus` bridging local `EventBus` with `PubSub` using origin `node_id` echo deduplication and `TraceContext` propagation.
5. **Durable Saga Stores (`rume-enterprise`)** — Implement `SqlSagaStore` (SQLite/Postgres/MySQL via `rume-data` + `rume_data::sql`) and `RedisSagaStore` (`redis` feature) implementing durable `SagaStore` with atomic CAS and execution claim leases alongside `InMemorySagaStore`.
6. **External Message Brokers (`rume-messaging`)** — Implement `MessageBroker` / `BrokerPublisher` / `BrokerConsumer` traits, `InMemoryBroker`, feature-gated `NatsBroker` (`nats` via `async-nats = "=0.53.0"`), and `RabbitMqBroker` (`rabbitmq` via `lapin = "=4.13.0"`), plus `OutboxRelay` broker dispatch integration.

## Current state (read before executing)

Facts verified against the tree at plan time:

- **Workspace pins (`Cargo.toml` / `deny.toml`)**:
  - Edition `2024`, MSRV `1.96`, `#![forbid(unsafe_code)]` on all library crates, `deny.toml` license allow-list (`Apache-2.0`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, etc.), `multiple-versions = "warn"`.
  - Already pinned in `Cargo.lock`: `reqwest = "=0.13.5"`, `rand = "=0.10.4"`, `chrono = "=0.4.45"`, `redis = "=1.8.3"`, `deadpool-redis = "=0.23.1"`, `sqlx = "=0.9.1"`, `tokio = "=1.54.1"`, `futures-util = "=0.3.34"`, `uuid = "=1.29.3"`, `tracing = "=0.1.44"`.
  - New crates to pin (all verified on crates.io against `deny.toml` and MSRV `1.96`):
    - `croner = "=5.0.1"` (`MIT`, MSRV `1.85`)
    - `hickory-resolver = "=0.26.4"` (`MIT OR Apache-2.0`, MSRV `1.88`)
    - `async-nats = "=0.53.0"` (`Apache-2.0`, MSRV `1.89.0`)
    - `lapin = "=4.13.0"` (`MIT`, MSRV `1.89.0`)
- **Messaging (`crates/messaging/src/`)**:
  - `jobs/` has `Job`, `QueuedJob`, `JobQueue`, `PostgresJobQueue`, `JobWorker`, `JobRegistry`, `RetryPolicy`, `IdempotencyStore`, `HeartbeatRegistry`. No `SqliteJobQueue`, `MysqlJobQueue`, `queue`/`priority` fields, or batch workflows yet.
  - `events/bus.rs` has `EventBus` with `publish_sync`, `publish_async`, `subscribe`, `subscribe_fn`. `event_bus_publish_sync_creates_span_and_propagates_trace_context` has a `tracing-core` callsite-interest flake when run concurrently with other tests.
  - `outbox/relay.rs` has `OutboxRelay` with `SqliteOutboxRepository`, `PostgresOutboxRepository`, `MysqlOutboxRepository`.
- **SQL assets (`crates/data/src/sql.rs` + `crates/data/sql/`)**:
  - Central home for all SQL DDL/DML constants (`PG_JOBS_*`, `SQLITE_OUTBOX_*`, `MYSQL_OUTBOX_*`, etc.).
- **Coordination (`crates/data/src/coordination/leader.rs`)**:
  - `LeaderElector` trait + `LeaderLease` implemented by `PostgresLeaderElector` and `RedisLeaderElector`.
- **HTTP (`crates/http/src/`)**:
  - `resilience/circuit_breaker.rs` provides `CircuitBreaker`, `CircuitBreakerConfig`, `TrialPermit`.
  - `rume_observability::TraceContext` (`traceparent`, `tracestate`) and `rume_foundation::telemetry::{RequestId, X_REQUEST_ID}` are available to `rume-http`.
  - No `client` module exists yet.
- **Enterprise (`crates/enterprise/src/saga/coordinator/store.rs`)**:
  - Defines `SagaStore` (`save_record_cas`, `load_record`, `list_incomplete`, `try_acquire_execution`, `release_execution`) and `InMemorySagaStore`. `rume-enterprise` does not yet depend on `rume-data`.
- **Facade (`crates/core/src/lib.rs`, `crates/core/src/prelude.rs`)**:
  - Re-exports `events`, `jobs`, `outbox`, `saga`, `resilience`, etc., and forwards `sqlite`, `postgres`, `mysql`, `redis`, `s3` features.

## Design Rulings

Autonomous engineering rulings for Milestone D (no open questions):

1. **`croner = "=5.0.1"` for cron scheduling:** Pin `croner = "=5.0.1"` (`MIT`, MSRV `1.85`) over `cron` (`0.17.0`) for active maintenance, 5/6-field cron support, and clean next-occurrence calculation. Use `chrono::FixedOffset` (already pinned) for timezone offsets without adding `chrono-tz`.
2. **Pure-Rust `async-nats = "=0.53.0"` and `lapin = "=4.13.0"` for external brokers:** Avoid `rdkafka` (C/CMake FFI, `unsafe` surface) to preserve `#![forbid(unsafe_code)]` and `deny.toml` cleanliness. Gate `NatsBroker` behind `nats` and `RabbitMqBroker` behind `rabbitmq` on `rume-messaging` and `rume`.
3. **Pure-Rust `hickory-resolver = "=0.26.4"` behind `dns-srv`:** Define a `ServiceDiscovery` trait in `rume-http::client` with `StaticDiscovery` always available and `DnsSrvDiscovery` gated behind `dns-srv` on `rume-http` and `rume`.
4. **`RumeClient` always-on in `rume-http::client`:** `reqwest = "=0.13.5"` is already in workspace dependencies; place `RumeClient` / `RumeClientBuilder` in `rume-http::client` integrating `TraceContext`, `X_REQUEST_ID`, `CircuitBreaker`, retries, and timeouts.
5. **Backward-compatible `JobQueue` evolution:** Add `queue: String` (default `"default"`) and `priority: i32` (default `0`, higher dequeued first) to `QueuedJob` and `Job` trait (with default methods). Add `JobQueue::dequeue_from(&self, worker_id: &str, queues: Option<&[&str]>)` with a default implementation delegating to `dequeue_for(worker_id)` so existing `JobQueue` implementations continue to compile.
6. **`rume-enterprise` -> `rume-data` DAG edge:** `rume-enterprise` (L3) depends on `rume-data` (L2) strictly downward in the 7-layer DAG, with `sqlite`, `postgres`, `mysql` features forwarded from `rume`. `SqlSagaStore` uses `rume_data::sql` constants and atomic `WHERE version = ?` CAS + lease-based execution claims.
7. **`tracing` callsite-interest flake fix:** Serialize `tracing::subscriber::set_default` tests in `crates/messaging/src/events/bus.rs` with a process-local `std::sync::Mutex` and invoke `tracing::callsite::rebuild_interest_cache()` before creating test spans so concurrent tests cannot leave stale `Interest::never()` cached on the `event.publish_sync` callsite.

## Global constraints

- **Workspace invariants**: `#![forbid(unsafe_code)]` in all library crates; Rust Edition 2024; MSRV `1.96`; concrete version pins (`=x.y.z`) in root `Cargo.toml`; all dependencies must pass `cargo deny check licenses` against `deny.toml`.
- **Strict 7-layer DAG**: L0 `rume-foundation` -> L1 `rume-observability` -> L2 `rume-data` -> L3 `rume-messaging`/`rume-security`/`rume-enterprise` -> L4 `rume-http` -> L5 `rume-app` -> L6 `rume` (`crates/core`).
- **Build concurrency**: Every `cargo` command MUST pass `-j 2`.
- **Zero-warning bar** (`rules/80-quality-bar.md`): `cargo fmt --all --check` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` must pass with zero warnings.
- **Execution mode (strictly sequential on `main`)**: Execute tasks `01` through `07` strictly in order on `main`, one implementer subagent at a time (`rules/30-git.md`, `rules/50-agent-conduct.md`).
- **TDD & fails-before** (`rules/50-agent-conduct.md`, `rules/80-quality-bar.md`): Task `01` scaffolds the compiling `todo!()` skeleton; tasks `02`–`06` write tests first against the skeleton (confirming fails-before), then implement minimal clean code; task `07` wires the facade and runs full end-to-end verification.
- **Commit discipline** (`rules/30-git.md`): Conventional Commits; no commits unless explicitly asked or announced up front by the controller.

## Constraints honoured

- **Job queue & worker lifecycle**: `crates/messaging/src/jobs/queue.rs` (`JobQueue` delegation contract), `crates/messaging/src/jobs/worker/runner.rs` (`JobWorker` admission/drain invariants), `crates/messaging/src/jobs/postgres.rs`.
- **SQL asset centralization**: `crates/data/src/sql.rs` (all SQL in `crates/data/sql/`, never inline cross-crate `include_str!`).
- **Leader election contract**: `crates/data/src/coordination/leader.rs` (`LeaderElector::try_acquire`, `renew`, `resign`).
- **Circuit breaker & W3C trace propagation**: `crates/http/src/resilience/circuit_breaker.rs` (`TrialPermit`), `crates/observability/src/trace_context.rs` (`TraceContext`, `TRACEPARENT`, `TRACESTATE`), `crates/foundation/src/telemetry.rs` (`RequestId`, `X_REQUEST_ID`).
- **Saga CAS & recovery invariants**: `crates/enterprise/src/saga/coordinator/store.rs` (`SagaStore::save_record_cas`, `try_acquire_execution`, `release_execution`) and `crates/enterprise/src/saga/coordinator/workflow.rs`.
- **Outbox relay contract**: `crates/messaging/src/outbox/relay.rs` (`OutboxRelay`, `RelayPollResult`).
- **Rules**: `rules/10-tech-stack.md`, `rules/40-verification.md`, `rules/50-agent-conduct.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.

## Task index

- `01-deps-and-skeleton.md` — Pin new crates, wire Cargo features and DAG edges, add SQL assets and queue/priority fields, fix the `EventBus` tracing flake, and scaffold compiling `todo!()` skeletons.
- `02-multi-dialect-job-queues-and-batches.md` — Implement `SqliteJobQueue`, `MysqlJobQueue`, named queues, priority ordering, per-queue concurrency caps, and `JobBatch` / `SqlBatchStore` workflows.
- `03-leader-elected-cron-scheduler.md` — Implement `CronScheduler` with `LeaderElector`, `croner`, timezone offsets, `MissedTickPolicy`, and graceful shutdown.
- `04-instrumented-outbound-http-client.md` — Implement `RumeClient` / `RumeClientBuilder` in `rume-http::client` with W3C trace & request-ID injection, `CircuitBreaker`, retries, timeouts, and `ServiceDiscovery` / `DnsSrvDiscovery`.
- `05-distributed-pubsub-and-event-bus.md` — Implement `PubSub` (`InMemoryPubSub`, `RedisPubSub`, `PostgresPubSub`) and `DistributedEventBus` with echo deduplication and trace propagation.
- `06-durable-saga-stores-and-brokers.md` — Implement `SqlSagaStore`, `RedisSagaStore`, `MessageBroker` (`InMemoryBroker`, `NatsBroker`, `RabbitMqBroker`), and `OutboxRelay` broker bridge.
- `07-facade-wiring-and-e2e.md` — Re-export Milestone D APIs in `rume` (`crates/core`), add `milestone_d_e2e.rs` and live container tests, and run full workspace verification.

## Done-when checklist

- [ ] `croner = "=5.0.1"`, `hickory-resolver = "=0.26.4"`, `async-nats = "=0.53.0"`, and `lapin = "=4.13.0"` are pinned in root `Cargo.toml` and `cargo deny check licenses` passes.
- [ ] `rume-messaging::events::bus::tests::event_bus_publish_sync_creates_span_and_propagates_trace_context` passes reliably under parallel test execution.
- [ ] `SqliteJobQueue`, `MysqlJobQueue`, and `PostgresJobQueue` support named `queue`, `priority` ordering, and `JobWorker` per-queue concurrency caps, plus `JobBatch` / `SqlBatchStore` workflows.
- [ ] `rume-messaging::cron::CronScheduler` executes recurring jobs once per cluster via `LeaderElector` with timezone support, `MissedTickPolicy`, and graceful shutdown.
- [ ] `rume-http::client::RumeClient` injects `traceparent`/`tracestate`/`X-Request-ID`, enforces `CircuitBreaker`, retries idempotent requests with exponential backoff + jitter, enforces timeouts, and supports `ServiceDiscovery` (`DnsSrvDiscovery` under `dns-srv`).
- [ ] `rume-messaging::pubsub` provides `PubSub` (`InMemoryPubSub`, `RedisPubSub`, `PostgresPubSub`) and `DistributedEventBus` with `origin_node_id` deduplication.
- [ ] `rume-enterprise::saga` provides durable `SqlSagaStore` (SQLite/Postgres/MySQL) and `RedisSagaStore` (`redis` feature) with atomic CAS and execution claim leases.
- [ ] `rume-messaging::broker` provides `MessageBroker` (`InMemoryBroker`, `NatsBroker` under `nats`, `RabbitMqBroker` under `rabbitmq`) and `OutboxRelay` broker integration.
- [ ] All verification commands below pass cleanly with zero warnings.

## Verification commands

```bash
cargo fmt --all --check
cargo deny check licenses
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

## Flagged gaps

- **Live NATS / RabbitMQ / Postgres / MySQL / Redis container tests**: `NatsBroker` and `RabbitMqBroker` compile-check and are verified at the trait/bridge level via `InMemoryBroker` hermetically; live container tests in `crates/core/tests/containers/` require Docker and are gated per `rules/testing.md` when Docker is unavailable.
