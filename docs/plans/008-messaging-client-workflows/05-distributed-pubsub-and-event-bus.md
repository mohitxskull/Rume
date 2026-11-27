# Task 05 — Distributed `PubSub` & `DistributedEventBus`

## Goal

Implement the `PubSub` trait (`InMemoryPubSub`, `RedisPubSub`, `PostgresPubSub`) in `rume-messaging::pubsub` and `DistributedEventBus` in `rume-messaging::events::distributed`, bridging local `EventBus` instances across nodes with `origin_node_id` echo deduplication and `TraceContext` propagation.

## Files touched

- `crates/messaging/src/pubsub/{mod.rs, memory.rs, redis.rs, postgres.rs}`
- `crates/messaging/src/events/{mod.rs, distributed.rs}`
- `crates/messaging/src/lib.rs`

## Deliverable

1. `PubSub` trait (`publish`, `subscribe`), `PubSubMessage`, `PubSubSubscription` (async `recv()` / stream), and `PubSubError`.
2. `InMemoryPubSub` (always available, `tokio::sync::broadcast`-backed) for hermetic multi-node testing.
3. `RedisPubSub` (`#[cfg(feature = "redis")]`) using `deadpool-redis` / `redis` async pub/sub.
4. `PostgresPubSub` (`#[cfg(feature = "postgres")]`) using `pg_notify` + `sqlx::postgres::PgListener` (`LISTEN`/`NOTIFY`).
5. `DistributedEventBus` wrapping a local `EventBus` + `Arc<dyn PubSub>` with a unique `node_id: Uuid`, publishing `DistributedEventEnvelope { origin_node_id, event_type, payload, trace_context }` and dispatching remote events to the local `EventBus` while ignoring envelopes whose `origin_node_id == self.node_id`.

## Constraints honoured

- **Local bus semantics**: `crates/messaging/src/events/bus.rs` (`EventBus::publish_sync`, `publish_async`, `subscribe`, `TraceContext` scoping).
- **No echo loops**: `DistributedEventBus` must drop inbound messages matching its own `origin_node_id`.
- **TDD (`rules/50-agent-conduct.md`)**: Write hermetic `InMemoryPubSub` tests first, confirm fails-before, then implement.

## Named tests (bodies omitted)

- `in_memory_pubsub_delivers_messages_to_multiple_subscribers` — verifies fan-out delivery and topic isolation.
- `distributed_event_bus_propagates_events_across_nodes_without_echo` — node A publishes an event; node B's local handler receives it once; node A does not re-receive its own remote echo.
- `distributed_event_bus_propagates_trace_context_across_nodes` — node B's listener observes node A's `trace_id`.

## Verification

```bash
cargo test -j 2 -p rume-messaging --lib --all-features
cargo clippy -j 2 -p rume-messaging --all-targets --all-features -- -D warnings
```

## Commit

Only when the user asks:

```text
feat(messaging): add PubSub backends and DistributedEventBus with echo deduplication
```

## Done-when

- [ ] `InMemoryPubSub`, `RedisPubSub` (`redis`), and `PostgresPubSub` (`postgres`) implement `PubSub`.
- [ ] `DistributedEventBus` bridges local `EventBus` and `PubSub` with `origin_node_id` deduplication and `TraceContext` propagation.
- [ ] All `rume-messaging` unit tests and clippy pass with zero warnings.
