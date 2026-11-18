# Task 03 — `rume-data` & `rume-messaging` (Inverting `outbox <-> persistence`)

## Goal

Create `rume-data` (`crates/data`: `persistence`, `cache`, `coordination`) and `rume-messaging` (`crates/messaging`: `events`, `outbox`, `jobs`), breaking the `outbox <-> persistence` cycle and replacing `CURRENT_TRACE_CONTEXT` scoping in `EventBus`, `OutboxRelay`, and `JobWorker` with OTel `TraceContextPropagator` + `OpenTelemetrySpanExt`.

## Files touched

- `Cargo.toml` (`[workspace.members]`: add `"crates/data"`, `"crates/messaging"`)
- `crates/data/Cargo.toml`
- `crates/data/src/lib.rs`
- `crates/data/src/persistence/` (moved from `crates/core/src/persistence/`, removing `crate::outbox::*` imports, moving `OutboxRepository` / `TransactionalOutbox` trait impls and non-DDL `sql/{sqlite,postgres,mysql}/outbox_*.sql` queries out to `rume-messaging`, and adding `MetricsRegistryPoolExt` for `MetricsRegistry::register_pool`)
- `crates/data/src/cache/` (moved from `crates/core/src/cache/`, using `rume_foundation::health`)
- `crates/data/src/coordination/` (moved from `crates/core/src/coordination/`)
- `crates/messaging/Cargo.toml`
- `crates/messaging/src/lib.rs`
- `crates/messaging/src/events/` (moved from `crates/core/src/events/`)
- `crates/messaging/src/outbox/` (moved from `crates/core/src/outbox/`, plus `impl OutboxRepository` and `impl TransactionalOutbox` for `rume_data::persistence::DatabasePool<...>` and the 15+ non-DDL `outbox_*.sql` query assets)
- `crates/messaging/src/jobs/` (moved from `crates/core/src/jobs/`)

## Deliverable

1. `rume-data` depends on `rume-foundation`, `rume-observability`, and `rume-macros` (`sql_assets!`, `embed_migrations!`), with **zero** dependency on `rume-messaging` (`outbox`). `outbox_schema.sql` / `Migrator::with_rume_core()` remains in `rume-data` without importing `outbox` Rust types.
2. `MetricsRegistryPoolExt` in `rume-data` provides `register_pool<B: DatabaseBackend + 'static>(&self, name: impl Into<String>, pool: &DatabasePool<B>)` on `rume_observability::MetricsRegistry`, resolving the orphan-rule conflict with `MetricsRegistry`.
3. `rume-messaging` depends on `rume-foundation`, `rume-observability`, and `rume-data`. Because `OutboxRepository` and `TransactionalOutbox` are local to `rume-messaging`, `impl OutboxRepository` and `impl TransactionalOutbox` for `DatabasePool<Sqlite/Postgres/MySql>` (along with `outbox_*.sql` queries) live cleanly in `rume-messaging` without violating Rust's orphan rule.
4. `EventBus::publish`, `OutboxMessage::new` / `OutboxRelay`, and `JobEnvelope` / `JobWorker` capture the active OTel `Context` via `TraceContext::current()` on enqueue/publish and restore parent context on the execution `tracing::Span` via `OpenTelemetrySpanExt::set_parent` (instead of `CURRENT_TRACE_CONTEXT.scope(...)`).

## Constraints honoured

- `crates/core/src/persistence/pool.rs`: `PoolStatus`, `export_metrics`, `HealthCheck`, and `Migrator::with_rume_core()` preserved.
- `crates/core/src/outbox/relay.rs`, `crates/core/src/jobs/worker.rs`, `crates/core/src/events/bus.rs`: W3C `traceparent` / `tracestate` propagation across async boundaries and panic/drain error propagation preserved.
- `crates/core/tests/integration/request_identity.rs`: Outbox message and queued job inherit the incoming HTTP `traceparent`.
- `crates/core/tests/integration/persistence.rs`: `SqlQuery`, `FromRow`, `embed_migrations!`, and `with_rume_core()` checksum validation preserved.

## Named tests (bodies omitted)

- `data_persistence_compiles_without_outbox_dependency` — unit test exercising `SqlPool` / `PoolStatus` / `HealthCheck` in `rume-data` with no `rume-messaging` dependency.
- `messaging_outbox_and_job_preserve_otel_trace_context_across_async_boundaries` — asserts `OutboxMessage` and `JobEnvelope` capture active `TraceContext`, and `OutboxRelay` / `JobWorker` attach a child `SpanContext` sharing the same `trace_id` to their execution spans.
- `messaging_event_bus_propagates_otel_parent_span_to_subscribers` — asserts `EventBus` subscriber execution runs inside a `tracing::Span` whose OTel `trace_id` matches the publisher's active span.

## Verification

```bash
cargo check -p rume-data --all-features -j 2
cargo test -p rume-data --features sqlite -j 2
cargo check -p rume-messaging --all-features -j 2
cargo test -p rume-messaging --features sqlite -j 2
```

Expected: `rume-data` and `rume-messaging` compile cleanly with 0 warnings and unit tests pass.

## Commit

Only when the user asks:

```text
refactor(data,messaging): extract rume-data and rume-messaging with OTel trace propagation
```

## Done-when

- [ ] `crates/data/Cargo.toml` has no dependency on `rume-messaging`.
- [ ] Both `crates/data/src/lib.rs` and `crates/messaging/src/lib.rs` declare `#![forbid(unsafe_code)]`.
- [ ] Zero references to `CURRENT_TRACE_CONTEXT` remain in `crates/messaging`.
- [ ] `cargo test -p rume-data --features sqlite -j 2` and `cargo test -p rume-messaging --features sqlite -j 2` pass.
