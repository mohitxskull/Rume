# Task 08 — Facade Wiring, `milestone_e_e2e.rs` & Full Workspace Verification

## Goal

Re-export all Milestone E modules and feature flags (`tls`, `acme`, `static-embed`, `mailer`, `grpc`, `graphql`, `scim`) in `rume` (`crates/core`), add `crates/core/tests/integration/milestone_e_e2e.rs` exercising every Milestone E capability end-to-end, and pass full workspace formatting, license, clippy, lib, integration, and doc-test verification.

## Files touched

- `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/core/src/prelude.rs`
- `crates/core/tests/integration.rs`, `crates/core/tests/integration/milestone_e_e2e.rs`

## Deliverable

- `rume` (`crates/core`) forwards `tls`, `acme`, `static-embed`, `mailer`, `grpc`, `graphql`, `scim` features (and `rume-security/{sqlite,postgres,mysql}` in `sqlite`/`postgres`/`mysql`) and re-exports `tls`, `realtime`, `static_files`, `http_cache`, `webhooks`, `mailer`, `flags`, `metering`, `scim` (under `scim`), plus feature-gated `grpc` (`rume_grpc`) and `graphql` (`rume_graphql`) modules and `prelude` items.
- `crates/core/tests/integration/milestone_e_e2e.rs` registered in `crates/core/tests/integration.rs` covering:
  - Native TLS (`TlsConfig::from_pem_bytes` + `RumeBuilder::tls`) HTTPS roundtrip + graceful shutdown drain;
  - `RealtimeHub` WebSocket + SSE tenant-scoped broadcast + graceful close/reconnect drain;
  - Static file serving + SPA fallback + `ETag`/`If-None-Match` `304` + `/v1` `Sunset`/`Deprecation` headers;
  - `CachedSecretProvider` TTL/rotation + `FeatureFlagStore` deterministic rollout + `UsageMeter` flush;
  - Outbound webhook HMAC-SHA256 signing + inbound `VerifyWebhookSignatureLayer` + `TestMailer` email capture;
  - SCIM 2.0 `/scim/v2/Users` & `/scim/v2/Groups` CRUD + RFC 7644 filter queries;
  - `rume-grpc` (`grpc` feature) and `rume-graphql` (`graphql` feature) integration over `Rume` / Axum.
- Full workspace verification passing with zero warnings.

## Constraints honoured

- `crates/core/src/lib.rs` (Stability Tiers & existing re-exports preserved).
- `crates/core/tests/integration.rs` (`#![deny(unsafe_code)]`, `testing` feature gate).
- `rules/40-verification.md` (run the whole check).
- `rules/80-quality-bar.md` (zero-warning bar).

## Named tests (bodies omitted)

- `integration::milestone_e_e2e::milestone_e_tls_realtime_caching_versioning_e2e` — boots a TLS `Rume` server with `RealtimeHub`, static/SPA fallback, `HttpCacheLayer`, and `/v1` deprecated route; verifies HTTPS request, WS/SSE tenant broadcast, `304 Not Modified`, `Sunset`/`Deprecation` headers, and clean shutdown drain.
- `integration::milestone_e_e2e::milestone_e_platform_scim_and_rpc_e2e` — exercises `CachedSecretProvider`, `SqlFeatureFlags` (SQLite), `SqlUsageMeter` + `BillingSink`, outbound/inbound HMAC webhooks, `TestMailer`, SCIM 2.0 `/scim/v2/Users` & `/scim/v2/Groups`, and `rume-grpc` + `rume-graphql` handlers.

## Verification

```bash
cargo fmt --all --check && \
cargo deny check licenses && \
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings && \
cargo test -j 2 --workspace --lib --all-features && \
cargo test -j 2 -p rume --test integration --all-features && \
cargo test -j 2 --workspace --doc --all-features
```
Expected: all commands exit 0 with zero warnings and zero test failures.

## Commit

Only when the user asks:
```text
feat(core): wire Milestone E facade exports and add milestone_e_e2e integration tests
```

## Done-when

- [ ] `rume` (`crates/core`) re-exports all Milestone E APIs and forwards all new features.
- [ ] `milestone_e_e2e.rs` passes under `cargo test -j 2 -p rume --test integration --all-features`.
- [ ] Full workspace `fmt`, `cargo deny check licenses`, `clippy --all-targets --all-features -- -D warnings`, `--lib`, `integration`, and `--doc` suites pass.
