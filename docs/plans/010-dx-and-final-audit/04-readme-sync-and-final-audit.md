# Task 04 — `README.md` Sync, Facade & Final Workspace Audit

## Goal

Synchronize `README.md` accurately with all shipped features from Milestones A–F, add `crates/core/tests/integration/milestone_f_e2e.rs` verifying `ApplicationMode::Task` and OpenAPI -> TypeScript SDK generation end-to-end, and pass the full workspace verification suite.

## Files touched

- `README.md`
- `crates/core/tests/integration.rs`, `crates/core/tests/integration/milestone_f_e2e.rs`

## Deliverable

- `README.md` updated to accurately reflect all shipped capabilities:
  - **What Rume provides**: HTTP, TLS/HTTP2/ACME, Real-time WebSockets & SSE, Static files / HTTP caching / API versioning, Observability (OTel, Prometheus, PII redaction), Security & Multi-Tenancy (Auth, CSRF, Feature Flags, Usage Metering, Dynamic Secrets), Persistence & Storage (SQLite/Postgres/MySQL, S3/Local), Messaging & Sagas (EventBus, Outbox, Jobs, Cron, NATS/RabbitMQ, Webhooks, Mailer), Enterprise (SAML, SCIM 2.0, gRPC, GraphQL), and CLI/DX (`new`, `doctor`, `dev`, `task`, `openapi`, `db`, `sql:check`, `list:routes`, `codegen`, `generate`, `make:migration`, `config`, `env`).
  - **Feature flags (`rume`)**: complete table matching `crates/core/Cargo.toml` (`sqlite`, `postgres`, `mysql`, `redis`, `otel`, `testing`, `http-client`, `dns-srv`, `nats`, `rabbitmq`, `s3`, `tls`, `acme`, `static-embed`, `mailer`, `saml`, `scim`, `enterprise`, `grpc`, `graphql`, `schemars`).
  - **Workspace layout**: complete table matching root `Cargo.toml` (including `crates/grpc/` and `crates/graphql/`, plus updated `crates/cli/` description).
- `crates/core/tests/integration/milestone_f_e2e.rs` registered in `crates/core/tests/integration.rs` exercising:
  - `ApplicationMode::Task` with `Rume::new()` + SQLite / `AppContext` + module lifecycle (`on_start` / `on_shutdown`) + task execution and unknown-task error handling.
  - `generate_openapi` -> `OpenApiSpec` JSON serialization roundtrip and structural validation of paths, params, request bodies, and responses.
- Full workspace audit passing with zero warnings.

## Constraints honoured

- `crates/core/Cargo.toml` (exact feature flag names).
- `Cargo.toml` (exact workspace member list).
- `crates/core/tests/integration.rs` (`#![deny(unsafe_code)]`, `testing` feature gate).
- `rules/40-verification.md` (run the whole check).
- `rules/80-quality-bar.md` (zero-warning bar, honest reports).

## Named tests (bodies omitted)

- `integration::milestone_f_e2e::milestone_f_task_mode_executes_with_context_and_clean_shutdown` — boots a `RumeBuilder` with a module lifecycle tracker and registered task in `ApplicationMode::Task`, verifies startup, task execution with `AppContext`, and clean shutdown without binding a listener.
- `integration::milestone_f_e2e::milestone_f_openapi_spec_exports_routes_and_schemas` — builds a `RumeBuilder` with typed routes (`HasOpenApiSchema` + path params), runs `ApplicationMode::OpenApi` / `RouteRegistry::to_openapi`, and verifies the resulting OpenAPI `3.1.0` JSON contains expected paths, parameters, and schemas.

## Verification

```bash
cargo fmt --all --check && \
cargo deny check licenses && \
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings && \
cargo check -j 2 -p rume --no-default-features && \
cargo test -j 2 --workspace --lib --all-features && \
cargo test -j 2 -p rume --test integration --all-features && \
cargo test -j 2 --workspace --doc --all-features
```
Expected: all commands exit 0 with zero warnings and zero test failures.

## Commit

Only when the user asks:
```text
docs,test: sync README.md with Milestones A-F and add milestone_f_e2e integration tests
```

## Done-when

- [ ] `README.md` accurately lists every shipped feature flag, workspace crate, and CLI command matching `Cargo.toml`, `crates/core/Cargo.toml`, and `crates/cli/src/cli.rs`.
- [ ] `milestone_f_e2e.rs` passes under `cargo test -j 2 -p rume --test integration --all-features`.
- [ ] Full workspace `fmt`, `cargo deny check licenses`, `clippy --all-targets --all-features -- -D warnings`, `--no-default-features`, `--lib`, `integration`, and `--doc` suites pass.
