# Plan 009 Review — Milestone E: Real-Time, TLS, Platform & Enterprise

- **Subject:** [`docs/plans/009-realtime-tls-platform/`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/README.md) (`README.md` + `01`–`08`)
- **Verdict:** **Changes Requested**
- **Findings:** 0 Critical, 2 Major, 3 Minor

---

## Executive Summary

Plan 009 is well-structured, adheres to `plan-authoring/SKILL.md` (clean vertical-slice task decomposition, no inline boilerplate or `Interfaces:` blocks, concrete `=x.y.z` pins, `-j 2` build concurrency, and clear done-when criteria), and preserves Rume's 7-layer DAG. In particular:
- `rume-grpc` (`crates/grpc`) and `rume-graphql` (`crates/graphql`) are cleanly isolated as optional L5 crates depending on `rume-foundation` (L0), `rume-observability` (L1), and `rume-http` (L4), gated behind `grpc` and `graphql` features on `rume` (`crates/core`).
- `rume-http` (L4) depending on `rume-messaging` (L3) and `rume-enterprise` (L3) is strictly downward in the DAG.
- Core dependencies (`rustls = "=0.23.46"`, `tokio-rustls = "=0.26.6"`, `hyper-util = "=0.1.21"`, `tonic = "=0.14.9"`) are already present in `Cargo.lock` and `cargo deny check licenses` passes cleanly against `deny.toml`.

Before Task 01 skeleton scaffolding begins, two **Major** gaps (TLS WebSocket upgrades / `ConnectInfo` / handshake timeouts in Task 02, and `rume-security` SQL feature wiring in Task 01/05/08) and three **Minor** items (`lettre` TLS backend, security hardening bounds, and `scim` feature scope) should be resolved in the plan.

---

## Findings

### Major

#### M-1: TLS accept loop must use `serve_connection_with_upgrades`, preserve `ConnectInfo<SocketAddr>`, and bound TLS handshakes
- **Where:** [`02-native-tls-http2-and-acme.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/02-native-tls-http2-and-acme.md#L15-L21), [`crates/app/src/app/builder/mod.rs`](file:///home/skull/Projects/rume/crates/app/src/app/builder/mod.rs#L672-L679)
- **Issue:**
  1. `hyper_util::server::conn::auto::Builder::serve_connection` does **not** support HTTP/1.1 `Upgrade: websocket`. Because Task 03 (`RealtimeHub`) and Task 08 (`milestone_e_tls_realtime_caching_versioning_e2e`) run WebSockets over a TLS `Rume` server, the TLS connection loop must use `serve_connection_with_upgrades` (or `serve_connection_with_upgrades` wrapped in `hyper_util::server::graceful::GracefulShutdown`).
  2. Plaintext `axum::serve` in `crates/app/src/app/builder/mod.rs` calls `.into_make_service_with_connect_info::<SocketAddr>()`. A manual `hyper_util` TLS accept loop will drop `ConnectInfo<SocketAddr>` unless the accepted peer `SocketAddr` is explicitly injected into the service/request extensions.
  3. Awaiting `tls_acceptor.accept(stream)` inline in the `TcpListener::accept()` loop allows a single stalled TLS handshake (Slowloris) to block all new connections. Handshakes must be spawned per connection (`tokio::spawn`) with a bounded handshake timeout.
  4. When `TlsConfig::acme(...)` is used, the `rustls-acme` background renewal driver task must be spawned alongside the server and aborted/stopped on shutdown, and the startup log (`listening on ...`) should reflect `https://` when TLS is active.
- **Required plan fix:** Add these four constraints explicitly to Task 02's `Deliverable` and `Constraints honoured`.

#### M-2: Missing `rume-security` SQL feature wiring (`sqlite`, `postgres`, `mysql`) in `crates/core/Cargo.toml` and Task 01
- **Where:** [`01-deps-and-skeleton.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/01-deps-and-skeleton.md#L12-L27), [`05-secret-provider-feature-flags-and-metering.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/05-secret-provider-feature-flags-and-metering.md#L24-L42), [`crates/security/Cargo.toml`](file:///home/skull/Projects/rume/crates/security/Cargo.toml#L44-L47), [`crates/core/Cargo.toml`](file:///home/skull/Projects/rume/crates/core/Cargo.toml#L38-L66)
- **Issue:** `crates/security/Cargo.toml` currently has no `sqlx` dependency and no `sqlite`, `postgres`, or `mysql` features. Task 05 introduces `SqlFeatureFlags` and `SqlUsageMeter` in `rume-security` (tested under `sqlite`), and Task 08 exercises them in `milestone_e_e2e.rs`. However, Task 01 omits `sqlite`/`postgres`/`mysql` from `rume-security` in its feature-wiring deliverable, and `crates/core/Cargo.toml` does not forward `rume-security/sqlite`, `rume-security/postgres`, `rume-security/mysql`. Without this forwarding, `cargo test -p rume --test integration` (with default `sqlite`, without `--all-features`) will not enable `rume-security/sqlite`.
- **Required plan fix:** Specify in Task 01 (and Task 08) that `crates/security/Cargo.toml` adds optional `sqlx` + `sqlite`/`postgres`/`mysql` features (matching `rume-messaging`/`rume-enterprise`), and `crates/core/Cargo.toml` forwards `rume-security/{sqlite,postgres,mysql}` in its `sqlite`, `postgres`, and `mysql` features.

---

### Minor

#### m-1: Pin `lettre` with `default-features = false` and `rustls` instead of default `native-tls` (OpenSSL)
- **Where:** [`README.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/README.md#L32), [`01-deps-and-skeleton.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/01-deps-and-skeleton.md#L25)
- **Issue:** `lettre = "=0.11.24"` with default features enables `tokio1-native-tls`, pulling in `native-tls` / `openssl-sys` and requiring system `libssl-dev`/`pkg-config`, whereas the rest of the workspace (`sqlx`, `reqwest`, `tokio-rustls`) uses `rustls`.
- **Recommendation:** Pin `lettre = { version = "=0.11.24", default-features = false, features = ["tokio1-rustls-tls", "smtp-transport", "builder", "hostname", "pool"] }` in root `Cargo.toml`.

#### m-2: Security hardening bounds for `RealtimeHub`, `VerifyWebhookSignatureLayer`, and SCIM filter parser
- **Where:** [`03-realtime-websockets-and-sse.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/03-realtime-websockets-and-sse.md#L16-L20), [`06-webhooks-and-transactional-email.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/06-webhooks-and-transactional-email.md#L22-L23), [`07-scim-and-optional-grpc-graphql.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/07-scim-and-optional-grpc-graphql.md#L17)
- **Issue / Recommendation:**
  1. **`RealtimeHub` (Task 03):** Validate/forbid `:` in `tenant_id` and `room` (or encode segments unambiguously) so `"realtime:{tenant_id}:{room}"` cannot suffer topic-delimiter collisions; bind `tenant_id` from the verified `RealtimeTicket`/session rather than trusting client-supplied parameters; redact `?ticket=` query parameters from logs/traces; handle `broadcast::RecvError::Lagged` explicitly.
  2. **`VerifyWebhookSignatureLayer` (Task 06):** Enforce a bounded `max_body_bytes` limit in `axum::body::to_bytes` (returning `413 Payload Too Large` on excess) to prevent unauthenticated OOM DoS; hex-decode `v1=` into `[u8; 32]` before constant-time comparison; reject both expired and future-skewed timestamps (`now.abs_diff(t) > max_age`).
  3. **SCIM RFC 7644 filter parser (Task 07):** Enforce `max_depth` and `max_filter_len` in the recursive-descent parser (returning `400 Bad Request` `ScimErrorResponse`) to prevent stack-overflow DoS on deeply nested parentheses.

#### m-3: Align `scim` feature gating scope across Tasks 01, 07, and 08
- **Where:** [`01-deps-and-skeleton.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/01-deps-and-skeleton.md#L27), [`07-scim-and-optional-grpc-graphql.md`](file:///home/skull/Projects/rume/docs/plans/009-realtime-tls-platform/07-scim-and-optional-grpc-graphql.md#L19)
- **Issue:** Task 01 lists `scim` across `rume-http, rume-app, rume-messaging, rume` (though `rume-messaging` and `rume-app` have no SCIM surface), and Task 07 leaves `rume-http::scim` feature gating ambiguous (`gated behind scim feature or always available in rume-http`).
- **Recommendation:** Gate `rume-http::scim` (and `dep:rume-enterprise` if optional in `rume-http`) behind `scim` on `rume-http` and `rume` (`crates/core`) consistently.

---

## Verification & Compliance Checklist
- [x] `plan-authoring/SKILL.md` folder layout, `README.md` sections, task file sections, and no-inline-code rule satisfied.
- [x] Workspace invariants (`#![forbid(unsafe_code)]`, Edition 2024, MSRV `1.96`, `=x.y.z` pins, `-j 2`) preserved.
- [x] 7-layer DAG preserved (`rume-http -> rume-messaging/rume-enterprise` downward; `rume-grpc`/`rume-graphql` at L5 gated on `rume`).
- [x] `cargo deny check licenses` passes on baseline workspace (`deny.toml`).
- [ ] Address M-1, M-2 (and m-1..m-3) before Task 01 skeleton scaffolding.
