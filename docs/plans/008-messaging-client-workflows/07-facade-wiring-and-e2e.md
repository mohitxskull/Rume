# Task 07 — Facade Wiring, E2E Integration Tests & Full Verification

## Goal

Re-export all new Milestone D APIs in `rume` (`crates/core/src/lib.rs` and `crates/core/src/prelude.rs`), add `crates/core/tests/integration/milestone_d_e2e.rs` and feature-gated live container tests in `crates/core/tests/containers/`, and run the full workspace verification suite.

## Files touched

- `crates/core/src/lib.rs`
- `crates/core/src/prelude.rs`
- `crates/core/tests/integration.rs`
- `crates/core/tests/integration/milestone_d_e2e.rs`
- `crates/core/tests/containers.rs`
- `crates/core/tests/containers/milestone_d.rs` (feature-gated `postgres`/`mysql`/`redis` tests)

## Deliverable

1. `rume` (`crates/core`) exposes `client` (`RumeClient`, `RumeClientBuilder`, `ServiceDiscovery`), `cron` (`CronScheduler`, `CronJobSpec`), `pubsub` (`PubSub`, `InMemoryPubSub`, `RedisPubSub`, `PostgresPubSub`), `broker` (`MessageBroker`, `InMemoryBroker`, `NatsBroker`, `RabbitMqBroker`), `DistributedEventBus`, `SqliteJobQueue`, `MysqlJobQueue`, `JobBatch`, `SqlBatchStore`, `SqlSagaStore`, and `RedisSagaStore` (with matching `prelude` re-exports for core types).
2. `crates/core/tests/integration/milestone_d_e2e.rs` verifies the complete Milestone D surface end-to-end without Docker (SQLite priority job queue + batch `then`/`finally`, `CronScheduler` leader election, `RumeClient` hitting an Axum test server with traceparent + circuit breaker + retry, `DistributedEventBus` two-node propagation, `SqlSagaStore` crash/recovery, and `OutboxRelay` -> `InMemoryBroker`).
3. `crates/core/tests/containers/milestone_d.rs` adds live container tests for `MysqlJobQueue`, `PostgresPubSub`, `RedisPubSub`, `SqlSagaStore` (Postgres/MySQL), and `RedisSagaStore` (respecting `rules/testing.md` container skip/availability policy).
4. Full workspace `fmt`, `deny`, `clippy`, unit tests, integration tests, and doc tests pass with zero warnings.

## Constraints honoured

- **Whole-workspace verification (`rules/40-verification.md`)**: Run every gate in `README.md` `## Verification commands` after the final edit.
- **No silent skips (`rules/testing.md`)**: Container tests use the existing `crates/core/tests/common/` helpers.

## Named tests (bodies omitted)

- `milestone_d_job_queue_priority_batch_and_cron_e2e` — exercises `SqliteJobQueue`, `JobBatch`, and `CronScheduler` through `rume::prelude`.
- `milestone_d_rume_client_tracing_resilience_e2e` — exercises `RumeClient` against an `axum` server verifying W3C `traceparent`, `X-Request-ID`, retry, and circuit breaker.
- `milestone_d_distributed_events_saga_and_broker_e2e` — exercises `DistributedEventBus`, `SqlSagaStore`, and `OutboxRelay` -> `InMemoryBroker`.

## Verification

```bash
cargo fmt --all --check
cargo deny check licenses
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```

## Commit

Only when the user asks:

```text
feat(core): wire Milestone D APIs into rume facade and add E2E tests
```

## Done-when

- [ ] `rume` and `rume::prelude` re-export all Milestone D public APIs.
- [ ] `milestone_d_e2e` integration tests pass via `cargo test -j 2 -p rume --test integration --all-features`.
- [ ] All six workspace verification commands pass with zero warnings.
