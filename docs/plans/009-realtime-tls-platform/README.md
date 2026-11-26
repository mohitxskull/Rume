# Plan 009 — Milestone E: Real-Time, TLS, Platform & Enterprise

## Goal

Close all Milestone E gaps — native TLS/HTTP2/ACME, real-time WebSockets & SSE, static file serving & HTTP caching & API versioning, platform building blocks (dynamic secrets, feature flags, usage metering, webhooks, transactional email), and enterprise SCIM 2.0 HTTP protocol plus optional gRPC & GraphQL integration — executed **strictly sequentially on `main`** (one task at a time):

1. **Native TLS, HTTP/2 & ACME (`rume-http` / `rume-app`)** — Implement `TlsConfig` (`from_pem_files`, `from_pem_bytes`, feature-gated `acme` via `rustls-acme`) using `tokio-rustls` + `hyper-util` (HTTP/1.1 + HTTP/2 ALPN) and wire `RumeBuilder::tls(...)` into `Rume::run()` / `Rume::run_with_listener_and_signal()` with clean graceful shutdown drain.
2. **Real-Time WebSockets & SSE `RealtimeHub` (`rume-http` + `rume-app`)** — Enable `axum/ws`, build `RealtimeHub` (local `tokio::sync::broadcast` + optional `rume_messaging::pubsub::PubSub` backplane with `origin_node_id` deduplication), ticket/session auth extractor, tenant-scoped rooms (`tenant_id`), configurable ping/pong heartbeat timeouts, and graceful shutdown drain (`Close` frames / SSE `reconnect` event).
3. **Static File Serving, HTTP Caching & API Versioning (`rume-http` + `rume-app`)** — Add `tower-http` `"fs"` static file serving + SPA fallback (plus optional `rust-embed` integration behind `static-embed`), `ETag` / `If-None-Match` / `If-Match` / `Cache-Control` middleware backed by `rume_data::cache::Cache`, `/v1` + header-based API version routing, and RFC 8594 `Sunset` / `Deprecation` response headers.
4. **Platform Building Blocks (`rume-foundation`, `rume-security`, `rume-messaging`, `rume-http`, `rume-testing`)** — Implement:
   - `SecretProvider` trait (`EnvSecretProvider`, `CachedSecretProvider` with TTL & rotation callback, `Vault`/`Kms` hook);
   - `FeatureFlagStore` (`InMemoryFeatureFlags`, `SqlFeatureFlags`) with tenant/user targeting and deterministic SHA-256 percentage rollout;
   - `UsageMeter` (`InMemoryUsageMeter`, `SqlUsageMeter`) with tenant-scoped counters and `BillingSink` trait;
   - Outbound & inbound Webhooks (`WebhookDispatcher` via `OutboxRelay`/`JobQueue` with HMAC-SHA256 `X-Rume-Signature` + timestamp, exponential backoff, delivery log; inbound `VerifyWebhookSignatureLayer` replay-protected middleware);
   - Transactional Email (`Mailer` trait, feature-gated `SmtpMailer` via `lettre` + `minijinja`, and `TestMailer` in `rume-testing`).
5. **Enterprise SCIM 2.0 HTTP Protocol & Optional RPC (`rume-enterprise`, `rume-http`, `rume-grpc`, `rume-graphql`)** — Implement RFC 7644 SCIM filter parser + `ScimStore` (`InMemoryScimStore`) + `/scim/v2/Users` and `/scim/v2/Groups` Axum handlers; add optional `rume-grpc` (`tonic`) and `rume-graphql` (`async-graphql` + `async-graphql-axum`) crates gated behind `grpc` and `graphql` features on `rume`, sharing W3C `TraceContext`, `AppError` mapping, and Axum router integration.

## Current state (read before executing)

Facts verified against the tree at plan time:

- **Workspace pins (`Cargo.toml` / `deny.toml`)**:
  - Edition `2024`, MSRV `1.96`, `#![forbid(unsafe_code)]` on all library crates, `deny.toml` license allow-list (`0BSD`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `MIT`, `MPL-2.0`, etc.), `multiple-versions = "warn"`.
  - New crates to pin (all verified on crates.io against `deny.toml` and MSRV `1.96`):
    - `tokio-rustls = "=0.26.6"` (`MIT OR Apache-2.0`, MSRV `1.71`)
    - `rustls = "=0.23.46"` (`Apache-2.0 OR ISC OR MIT`, MSRV `1.71`)
    - `rustls-pki-types = "=1.16.0"` (`MIT OR Apache-2.0`, MSRV `1.60`)
    - `rustls-pemfile = "=2.2.0"` (`Apache-2.0 OR ISC OR MIT`)
    - `hyper-util = "=0.1.21"` (`MIT`, MSRV `1.85`)
    - `rustls-acme = "=0.15.4"` (`Apache-2.0 OR MIT`)
    - `rust-embed = "=8.14.0"` (`MIT`, MSRV `1.80.0`)
    - `lettre = "=0.11.24"` (`MIT`, MSRV `1.85`, with `default-features = false, features = ["tokio1-rustls-tls", "smtp-transport", "builder", "hostname", "pool"]`)
    - `minijinja = "=3.1.0"` (`Apache-2.0`, MSRV `1.70`)
    - `tonic = "=0.14.9"` (`MIT`, MSRV `1.88`)
    - `prost = "=0.14.6"` (`Apache-2.0`, MSRV `1.85`)
    - `tonic-prost = "=0.14.9"` (`MIT`, MSRV `1.88`)
    - `async-graphql = "=7.2.1"` (`MIT OR Apache-2.0`, MSRV `1.89.0`)
    - `async-graphql-axum = "=7.2.1"` (`MIT OR Apache-2.0`)
- **Application server (`crates/app/src/app/builder/mod.rs`)**:
  - `RumeBuilder::run` / `run_with_signal` / `run_with_listener_and_signal` serve plaintext `axum::serve(listener, ...)` with `watch::channel(false)` shutdown latching and `LifecycleState::Terminating` / `AvailabilityState::RefusingTraffic` transitions. No TLS or ACME support yet.
- **HTTP (`crates/http/src/`)**:
  - `axum` currently enables only `["multipart"]`; `tower-http` enables `["cors", "compression-gzip", "compression-br", "compression-zstd", "limit"]` (no `"fs"` yet).
  - `routing/group.rs` provides `RouteGroup` with URL prefixing, but no API version matcher or `Sunset`/`Deprecation` header middleware.
  - No `realtime`, `static_files`, `http_cache`, `webhooks`, or `scim` modules exist yet.
- **Foundation (`crates/foundation/src/env/secrets.rs`)**:
  - Provides secret-key detection and redaction (`is_secret_key`, `redact_value`), but no dynamic `SecretProvider` trait or TTL-cached rotation hook.
- **Enterprise (`crates/enterprise/src/enterprise/scim.rs`)**:
  - Defines RFC 7643 models (`ScimUser`, `ScimGroup`, `ScimListResponse`, `ScimErrorResponse`), but no RFC 7644 filter parser, `ScimStore`, or HTTP handlers.
- **Data (`crates/data/src/sql.rs` & `crates/data/src/cache/`)**:
  - Centralizes all SQL DDL/DML in `crates/data/sql/` via `rume_data::sql` and provides `Cache` trait (`InMemoryCache`, `RedisCache`).

## Design Rulings

Autonomous engineering rulings for Milestone E (no open questions):

1. **`tokio-rustls` + `hyper-util` over `axum-server` for native TLS & HTTP/2:** `RumeBuilder::run_with_listener_and_signal` already owns `TcpListener` binding, `run_startup`, `LifecycleState::Terminating` / `AvailabilityState::RefusingTraffic` transitions, and `run_shutdown`. Accepting TLS connections with `tokio_rustls::TlsAcceptor` and serving via `hyper_util::server::conn::auto::Builder` (HTTP/1.1 + HTTP/2 ALPN) preserves `Rume`'s exact lifecycle, pre-bound listener testing, and `#![forbid(unsafe_code)]` without duplicating server ownership.
2. **Feature-gated ACME (`acme`), embedded assets (`static-embed`), SMTP (`mailer`), gRPC (`grpc`), and GraphQL (`graphql`):** Keep default builds lean; gate `rustls-acme` behind `acme` (implies `tls`), `rust-embed` behind `static-embed`, `lettre` + `minijinja` behind `mailer`, `tonic` + `prost` + `tonic-prost` behind `grpc`, and `async-graphql` + `async-graphql-axum` behind `graphql`.
3. **7-Layer DAG preservation for `RealtimeHub` and `scim`:** `rume-http` (L4) may depend on `rume-messaging` (L3) and `rume-enterprise` (L3) strictly downward in the DAG. `RealtimeHub` bridges `rume_messaging::pubsub::PubSub` directly in `rume-http::realtime`, and `rume-http::scim` mounts Axum handlers backed by `rume_enterprise::enterprise::scim::ScimStore`.
4. **Deterministic SHA-256 bucketing for Feature Flags:** Use `sha2::Sha256` (already pinned in `rume-security`) over `(flag_key, subject_id)` modulo `100` for stable, cross-process, cross-restart percentage rollouts without adding another hash dependency.
5. **HMAC-SHA256 webhook signature format & replay window:** Use `X-Rume-Signature: t=<unix_secs>,v1=<hex_hmac_sha256>` signing `"{t}.{body}"` verified in constant time via `subtle::ConstantTimeEq` with a default `300`-second replay tolerance.
6. **RFC 7644 SCIM filter AST in `rume-enterprise`:** Implement a pure-Rust recursive-descent parser for RFC 7644 §3.4.2.2 filters (`eq`, `ne`, `co`, `sw`, `ew`, `pr`, `gt`, `ge`, `lt`, `le`, `and`, `or`, `not`, parentheses) in `rume-enterprise::enterprise::scim` with hermetic unit tests, keeping HTTP routing and status mapping in `rume-http::scim`.
7. **Workspace crates `crates/grpc` (`rume-grpc`) and `crates/graphql` (`rume-graphql`):** Place optional RPC integrations in dedicated L4/L5 workspace crates (`rume-grpc`, `rume-graphql`) depending on `rume-foundation`, `rume-observability`, and `rume-http`, re-exported by `rume` (`crates/core`) under `grpc` and `graphql` features so users who do not enable RPC pay zero compile cost.

## Global constraints

- **Workspace invariants**: `#![forbid(unsafe_code)]` in all library crates; Rust Edition 2024; MSRV `1.96`; concrete version pins (`=x.y.z`) in root `Cargo.toml`; all dependencies must pass `cargo deny check licenses` against `deny.toml`.
- **Strict 7-layer DAG**: L0 `rume-foundation` -> L1 `rume-observability` -> L2 `rume-data` -> L3 `rume-messaging`/`rume-security`/`rume-enterprise` -> L4 `rume-http` -> L5 `rume-app` -> L6 `rume` (`crates/core`).
- **Build concurrency**: Every `cargo` command MUST pass `-j 2`.
- **Zero-warning bar** (`rules/80-quality-bar.md`): `cargo fmt --all --check` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` must pass with zero warnings.
- **Execution mode (strictly sequential on `main`)**: Execute tasks `01` through `08` strictly in order on `main`, one implementer subagent at a time (`rules/30-git.md`, `rules/50-agent-conduct.md`).
- **TDD & fails-before** (`rules/50-agent-conduct.md`, `rules/80-quality-bar.md`): Task `01` scaffolds the compiling `todo!()` skeleton; tasks `02`–`07` write tests first against the skeleton (confirming fails-before), then implement minimal clean code; task `08` wires the facade and runs full end-to-end verification.
- **Commit discipline** (`rules/30-git.md`): Conventional Commits; no commits unless explicitly asked or announced up front by the controller.

## Constraints honoured

- **Application lifecycle & shutdown**: `crates/app/src/app/builder/mod.rs` (`RumeBuilder::run_with_listener_and_signal`), `crates/app/src/app/lifecycle.rs` (`run_startup`, `run_shutdown`).
- **SQL asset centralization**: `crates/data/src/sql.rs` (all SQL in `crates/data/sql/`, never inline cross-crate `include_str!`).
- **W3C trace & request-ID propagation**: `crates/observability/src/trace_context.rs` (`TraceContext`), `crates/foundation/src/telemetry.rs` (`RequestId`, `X_REQUEST_ID`).
- **Cache & Outbox contracts**: `crates/data/src/cache/mod.rs` (`Cache`), `crates/messaging/src/outbox/relay.rs` (`OutboxRelay`), `crates/messaging/src/pubsub/mod.rs` (`PubSub`).
- **SCIM 2.0 models**: `crates/enterprise/src/enterprise/scim.rs` (`ScimUser`, `ScimGroup`, `ScimListResponse`, `ScimErrorResponse`).
- **Rules**: `rules/10-tech-stack.md`, `rules/40-verification.md`, `rules/50-agent-conduct.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.

## Task index

- `01-deps-and-skeleton.md` — Pin Milestone E crates, add `crates/grpc` and `crates/graphql`, wire features and DAG edges, add SQL assets, and scaffold compiling `todo!()` skeletons.
- `02-native-tls-http2-and-acme.md` — Implement `TlsConfig` (PEM + `rustls-acme`), HTTP/1.1 + HTTP/2 ALPN serving via `tokio-rustls` + `hyper-util`, and `RumeBuilder::tls(...)` graceful shutdown drain.
- `03-realtime-websockets-and-sse.md` — Implement `RealtimeHub` (WebSocket + SSE, `PubSub` backplane, ticket/session auth, tenant rooms, ping/pong heartbeats, graceful close/reconnect drain) and `RumeBuilder::realtime(...)`.
- `04-static-files-http-caching-and-api-versioning.md` — Implement static file serving + SPA fallback (`tower-http/fs` + optional `rust-embed`), `ETag`/`If-None-Match`/`If-Match`/`Cache-Control` middleware backed by `Cache`, and API versioning + RFC 8594 `Sunset`/`Deprecation` headers.
- `05-secret-provider-feature-flags-and-metering.md` — Implement `SecretProvider` (`EnvSecretProvider`, `CachedSecretProvider`, `Vault`/`Kms` hook), `FeatureFlagStore` (`InMemoryFeatureFlags`, `SqlFeatureFlags`, SHA-256 rollout), and `UsageMeter` (`InMemoryUsageMeter`, `SqlUsageMeter`, `BillingSink`).
- `06-webhooks-and-transactional-email.md` — Implement outbound & inbound HMAC-SHA256 webhooks (`WebhookDispatcher` + `VerifyWebhookSignatureLayer`) and transactional email (`Mailer`, `SmtpMailer` via `lettre` + `minijinja`, `TestMailer`).
- `07-scim-and-optional-grpc-graphql.md` — Implement RFC 7644 SCIM filter parser + `ScimStore` + `/scim/v2/Users` & `/scim/v2/Groups` handlers, plus `rume-grpc` (`tonic`) and `rume-graphql` (`async-graphql`) crates.
- `08-facade-wiring-and-e2e.md` — Re-export Milestone E APIs and features in `rume` (`crates/core`), add `milestone_e_e2e.rs`, and run full workspace verification.

## Done-when checklist

- [ ] All new dependencies are pinned with `=x.y.z` in root `Cargo.toml` and `cargo deny check licenses` passes.
- [ ] `RumeBuilder::tls(...)` serves HTTPS (HTTP/1.1 + HTTP/2 ALPN) from PEM files and supports feature-gated `acme` (`rustls-acme`) with graceful shutdown drain.
- [ ] `rume-http::realtime::RealtimeHub` supports WebSockets (`axum/ws`) and SSE, optional `PubSub` backplane, ticket/session auth, tenant-scoped rooms, ping/pong heartbeats, and graceful shutdown drain.
- [ ] `rume-http` provides static file serving + SPA fallback (and `rust-embed` under `static-embed`), `ETag`/`If-None-Match`/`If-Match`/`Cache-Control` caching middleware backed by `rume_data::cache::Cache`, `/v1` + header API versioning, and RFC 8594 `Sunset`/`Deprecation` headers.
- [ ] `rume-foundation` provides `SecretProvider` (`EnvSecretProvider`, `CachedSecretProvider`, `Vault`/`Kms` hook); `rume-security` provides `FeatureFlagStore` (`InMemoryFeatureFlags`, `SqlFeatureFlags`) and `UsageMeter` (`InMemoryUsageMeter`, `SqlUsageMeter`, `BillingSink`).
- [ ] `rume-messaging` + `rume-http` provide outbound/inbound HMAC-SHA256 webhooks and `Mailer` / `SmtpMailer` (`mailer` feature), with `TestMailer` in `rume-testing`.
- [ ] `rume-enterprise` + `rume-http` provide RFC 7644 SCIM 2.0 filter parsing, `ScimStore`, and `/scim/v2/Users` & `/scim/v2/Groups` CRUD handlers.
- [ ] `rume-grpc` (`grpc` feature) and `rume-graphql` (`graphql` feature) compile cleanly, propagate W3C `TraceContext`, map `AppError`, and integrate with Axum.
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

- **Live ACME CA & external SMTP / Vault servers**: ACME (`rustls-acme`) and SMTP (`SmtpMailer`) compile-check and are verified at the config/trait/renderer level hermetically; `TestMailer` covers email dispatch assertions in integration tests, while live Let's Encrypt CA and external SMTP/Vault servers require external network infrastructure.
