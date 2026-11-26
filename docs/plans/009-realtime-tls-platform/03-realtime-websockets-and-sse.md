# Task 03 — Real-Time WebSockets & SSE (`RealtimeHub`)

## Goal

Build `RealtimeHub` in `rume-http::realtime` supporting WebSockets (`axum/ws`) and Server-Sent Events (SSE), optional `rume_messaging::pubsub::PubSub` backplane for multi-instance fan-out, ticket/session auth extraction, tenant-scoped rooms, configurable ping/pong heartbeat timeouts, and graceful shutdown drain, wired into `RumeBuilder`.

## Files touched

- `crates/http/src/realtime/mod.rs` (and submodules `hub.rs`, `ws.rs`, `sse.rs`, `auth.rs`)
- `crates/app/src/app/builder/mod.rs`

## Deliverable

- `RealtimeConfig` (max message size, channel capacity, `heartbeat_interval`, `heartbeat_timeout`, `shutdown_drain_timeout`).
- `RealtimeHub`:
  - Local `tokio::sync::broadcast` channels keyed by `(tenant_id, room)`.
  - Optional `PubSub` backplane (`RealtimeHub::with_pubsub(Arc<dyn PubSub>)`) publishing to `"realtime:{tenant_id}:{room}"` and ignoring messages matching `origin_node_id` to prevent echo loops; validate/forbid `:` in `tenant_id` and `room` (rejecting invalid identifiers) so topic segments cannot collide.
  - `publish(tenant_id, room, event)` and `subscribe(tenant_id, room)`, handling `broadcast::RecvError::Lagged` explicitly (emitting a lag warning/event rather than silently dropping or terminating).
  - `shutdown()` / cancellation token integration so active WebSocket connections receive a `Close` frame and active SSE streams receive `event: reconnect` before closing cleanly.
- Auth & tenancy extractors (`RealtimeTicket` / `RealtimeAuth`) validating bearer/query ticket or `rume_security::auth` session and binding `tenant_id` from the verified ticket/session (never trusting client-supplied tenant parameters) so cross-tenant room access is rejected (`403 Forbidden` / `401 Unauthorized`). Redact `?ticket=` query parameters from logs/traces.
- Axum handlers (`ws_handler`, `sse_handler`) with WebSocket ping/pong heartbeat loop (closing stalled clients after `heartbeat_timeout`).
- `RumeBuilder::realtime(RealtimeHub)` attaching the hub to application state/extensions.

## Constraints honoured

- `crates/messaging/src/pubsub/mod.rs` (`PubSub` trait, `PubSubMessage`).
- `crates/security/src/tenant/model.rs` (`TenantId` / tenant isolation).
- `crates/app/src/app/builder/mod.rs` (`AvailabilityState::RefusingTraffic` / shutdown coordination).
- `#![forbid(unsafe_code)]`, `rules/80-quality-bar.md`.

## Named tests (bodies omitted)

- `rume_http::realtime::tests::ws_and_sse_receive_tenant_scoped_broadcasts_and_isolate_tenants` — subscribes two clients in different tenants to the same room name and asserts messages only reach the matching tenant.
- `rume_http::realtime::tests::realtime_hub_with_pubsub_deduplicates_origin_node_id` — wires `InMemoryPubSub` across two `RealtimeHub` instances and asserts cross-hub delivery without local echo duplication.
- `rume_http::realtime::tests::ws_heartbeat_timeout_closes_unresponsive_client` — configures a short `heartbeat_timeout`, stalls pong responses, and asserts the server closes the connection.
- `rume_http::realtime::tests::realtime_graceful_shutdown_sends_ws_close_and_sse_reconnect` — triggers hub shutdown and asserts WS receives a `Close` frame and SSE emits `event: reconnect` before stream termination.
- `rume_http::realtime::tests::realtime_auth_rejects_missing_or_cross_tenant_ticket` — verifies `401`/`403` when ticket is missing or targets another tenant.

## Verification

```bash
cargo test -j 2 -p rume-http -p rume-app --all-features
```
Expected: all `realtime` and `rume-app` tests pass with zero warnings.

## Commit

Only when the user asks:
```text
feat(http,app): add RealtimeHub with WebSockets, SSE, PubSub backplane, heartbeats, and drain
```

## Done-when

- [ ] `RealtimeHub` supports WebSockets (`axum/ws`), SSE, and optional `PubSub` backplane with `origin_node_id` deduplication.
- [ ] Ticket/session auth enforces tenant-scoped room access.
- [ ] Ping/pong heartbeat timeouts and graceful shutdown drain (`Close` / SSE `reconnect`) are verified by tests.
