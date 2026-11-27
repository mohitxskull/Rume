# Task 06 — Durable Saga Stores (`SqlSagaStore`, `RedisSagaStore`) & External Message Brokers

## Goal

Implement durable `SagaStore` backends (`SqlSagaStore` for SQLite/Postgres/MySQL and `RedisSagaStore` under `redis`) in `rume-enterprise::saga`, and implement `MessageBroker` / `BrokerPublisher` / `BrokerConsumer` (`InMemoryBroker`, `NatsBroker` under `nats`, `RabbitMqBroker` under `rabbitmq`) plus `OutboxRelay` broker dispatch integration in `rume-messaging::broker`.

## Files touched

- `crates/enterprise/src/saga/coordinator/{mod.rs, sql_store.rs, redis_store.rs}`
- `crates/messaging/src/broker/{mod.rs, memory.rs, nats.rs, rabbitmq.rs}`
- `crates/messaging/src/outbox/relay.rs` (add `OutboxRelay::with_broker` / `broker_dispatcher` helper)

## Deliverable

1. `SqlSagaStore` in `rume-enterprise::saga` supporting SQLite (`sqlite`), Postgres (`postgres`), and MySQL (`mysql`) via `rume_data::sql::{SQLITE_SAGA_*, PG_SAGA_*, MYSQL_SAGA_*}`, enforcing atomic `WHERE version = ?` CAS updates and lease-fenced execution claims (`try_acquire_execution`, `release_execution`).
2. `RedisSagaStore` (`#[cfg(feature = "redis")]`) using `deadpool-redis` + atomic Lua scripts for CAS and `SET NX PX` / token-checked `DEL` execution claim leases.
3. `MessageBroker`, `BrokerPublisher`, `BrokerConsumer`, `BrokerMessage`, and `BrokerError` in `rume-messaging::broker`, with `InMemoryBroker` (always available), `NatsBroker` (`#[cfg(feature = "nats")]` via `async-nats`), and `RabbitMqBroker` (`#[cfg(feature = "rabbitmq")]` via `lapin`).
4. Helper bridging `OutboxRelay` to any `Arc<dyn BrokerPublisher>` so outbox messages publish to NATS, RabbitMQ, or `InMemoryBroker` with `TraceContext` headers.

## Constraints honoured

- **Saga CAS & claim contract**: `crates/enterprise/src/saga/coordinator/store.rs` (`SagaStore`, `SagaRecord`, `SagaStatus`) and `crates/enterprise/src/saga/coordinator/workflow.rs` (`SagaRecoveryHandle`).
- **Outbox relay contract**: `crates/messaging/src/outbox/relay.rs` (`OutboxRelay::new`, `OutboxMessage`).
- **TDD (`rules/50-agent-conduct.md`)**: Write hermetic SQLite `SqlSagaStore` and `InMemoryBroker` + `OutboxRelay` tests first, confirm fails-before, then implement.

## Named tests (bodies omitted)

- `sql_saga_store_enforces_cas_and_recovers_incomplete_sagas` — verifies `version` conflict error, persistence across store instances, and `list_incomplete`.
- `sql_saga_store_fences_concurrent_execution_claims` — second `try_acquire_execution` for the same `saga_id` fails until `release_execution` is called.
- `in_memory_broker_publishes_and_consumes_with_headers` — verifies topic publish/subscribe and header/traceparent preservation.
- `outbox_relay_publishes_to_message_broker` — saves an `OutboxMessage` in SQLite, runs `OutboxRelay::poll_and_dispatch`, and verifies receipt on `InMemoryBroker`.

## Verification

```bash
cargo test -j 2 -p rume-enterprise --lib --all-features
cargo test -j 2 -p rume-messaging --lib --all-features
cargo clippy -j 2 -p rume-enterprise -p rume-messaging --all-targets --all-features -- -D warnings
```

## Commit

Only when the user asks:

```text
feat(enterprise,messaging): add durable SqlSagaStore/RedisSagaStore and MessageBroker backends
```

## Done-when

- [ ] `SqlSagaStore` (SQLite/Postgres/MySQL) and `RedisSagaStore` (`redis`) implement `SagaStore`.
- [ ] `InMemoryBroker`, `NatsBroker` (`nats`), `RabbitMqBroker` (`rabbitmq`), and `OutboxRelay` broker bridge compile and pass tests.
- [ ] Zero clippy warnings in `rume-enterprise` and `rume-messaging`.
