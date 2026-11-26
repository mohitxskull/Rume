# Rume

Rume is an explicit, typed, production-ready application shell for [Axum](https://github.com/tokio-rs/axum).

> **Work in progress — not stable.** Rume is pre-1.0 (`0.1.0`, Rust Edition 2024, MSRV `1.96`). Public APIs, configuration schemas, and behavioral contracts may change between releases without notice.

## What Rume provides

Rume is an application shell, not a magic runtime container:

- **Progressive complexity** — start with a minimal Axum-backed app, add typed modules, persistence, and background workers when needed
- **Explicit by construction** — dependencies, transactions, middleware, and lifecycle hooks live in plain Rust code (no classpath scanning, no AOP, no hidden DI container)
- **First-class modules** — bounded domains with encapsulated state, explicit DAG dependencies, route collision checks, and topological startup/shutdown lifecycle
- **HTTP, TLS & real-time** — `RouteSpec` routing with path/query matchers, RFC 9457 Problem Details, OpenAPI v3.1 & Docs UI, native TLS / HTTP/2 / ACME, WebSockets & SSE (`RealtimeHub`), static file serving (`static-embed`), HTTP caching (`ETag` / `304`), and API versioning (`Sunset` / `Deprecation`)
- **Typed configuration & secrets** — layered TOML/env config validated at boot, `secrecy` redaction in logs/diagnostics, and dynamic `SecretProvider` caching/rotation
- **Persistence & storage** — backend traits for SQLite, PostgreSQL, and MySQL with transactional migrations, checksums, rollback ordering, SQL validation (`sqlparser`), and S3 / local object storage
- **Messaging & resilience** — `EventBus`, transactional outbox, background job worker with lease heartbeats and DLQ, cron scheduler, NATS & RabbitMQ brokers, HMAC-signed webhooks, transactional mailer, distributed locks/leader election, circuit breaker (`TrialPermit`), sliding-window rate limiter, and singleflight cache
- **Security & multi-tenancy** — session/API-key/bearer/OIDC auth, CSRF & security headers, tenant-scoped caching/quotas, feature flags, and usage metering
- **Observability** — OpenTelemetry tracing/metrics/logs (`tracing-opentelemetry`, W3C trace propagation, Prometheus/OTLP), PII redaction, and structured health/readiness probes
- **Enterprise & RPC** — optional SAML 2.0, SCIM 2.0, Saga workflows, gRPC (`rume-grpc`), and GraphQL (`rume-graphql`)
- **CLI & developer experience** — `rume` CLI for scaffolding (`new`), diagnostics (`doctor`), live reload (`dev`), one-off app tasks (`task`), OpenAPI export & TypeScript SDK codegen (`openapi`), migrations (`db`, `make:migration`), SQL checks (`sql:check`), route inspection (`list:routes`), codegen (`codegen`, `generate`), and config/env inspection (`config`, `env`, `completions`)

## Quick start

```rust
use rume::Rume;

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    Rume::new()
        .get("/", || async { "Hello, Rume!" })
        .run()
        .await
}
```

Or scaffold a new project with the CLI:

```bash
cargo run -p rume-cli -- new my-app --dialect sqlite # or postgres, mysql
```

## Feature flags (`rume`)

Default features: `sqlite`.

| Feature | Enables |
|---|---|
| `sqlite` | SQLite persistence, migrations, and outbox |
| `postgres` | PostgreSQL persistence, migrations, outbox, and distributed coordination |
| `mysql` | MySQL/MariaDB persistence, migrations, and outbox |
| `redis` | Redis caching, distributed locks/leader election, rate limiting, sessions, and quotas |
| `otel` | OpenTelemetry OTLP/metrics integration in `rume-observability` |
| `testing` | `rume-testing` (`TestApp`, `TestServer`, database/container harnesses) |
| `http-client` | Outbound HTTP client support in `rume-security` (OIDC/JWKS) |
| `dns-srv` | DNS SRV service discovery in `rume-http` |
| `nats` | NATS messaging transport in `rume-messaging` |
| `rabbitmq` | RabbitMQ (AMQP) messaging transport in `rume-messaging` |
| `s3` | S3-compatible object storage backend in `rume-data` |
| `tls` | Native TLS (`rustls`) and HTTP/2 server support in `rume-app` and `rume-http` |
| `acme` | Automatic ACME / Let's Encrypt TLS certificate provisioning (implies `tls`) |
| `static-embed` | Embedded static asset serving (`rust-embed`) in `rume-app` and `rume-http` |
| `mailer` | Transactional SMTP emailer and MiniJinja templates in `rume-messaging` |
| `saml` | *(Experimental)* SAML 2.0 assertion parsing & validation via caller-supplied `SamlSignatureVerifier` |
| `scim` | SCIM 2.0 user/group provisioning routes and filters in `rume-http` |
| `enterprise` | *(Experimental)* `saml` + SCIM models and Saga orchestration |
| `grpc` | Optional gRPC server/interceptor integration (`rume-grpc`) |
| `graphql` | Optional GraphQL schema/route integration (`rume-graphql`) |
| `schemars` | JSON Schema generation support in `rume-foundation` |

## Workspace layout

| Path | Crate / Purpose |
|---|---|
| `crates/core/` | `rume` — umbrella facade crate and integration/container test suites |
| `crates/foundation/` | `rume-foundation` — errors, config, env, secrets, health, helpers |
| `crates/observability/` | `rume-observability` — OpenTelemetry traces, W3C context, Prometheus/OpenMetrics exposition, PII redaction |
| `crates/data/` | `rume-data` — persistence backends, migrations, typed/singleflight cache, locks & leader election, object storage |
| `crates/messaging/` | `rume-messaging` — event bus, transactional outbox, job queue & worker, cron, NATS/RabbitMQ, webhooks, mailer |
| `crates/security/` | `rume-security` — auth, CSRF, headers, multi-tenancy, tenant-scoped cache/quotas, feature flags, usage metering |
| `crates/http/` | `rume-http` — routing, route registry, transport middleware (CORS/compression/body limits), OpenAPI & Docs UI, validation, circuit breaker, rate limiter, realtime WS/SSE, static files, HTTP cache, SCIM |
| `crates/enterprise/` | `rume-enterprise` — SAML, SCIM, and Saga workflows |
| `crates/app/` | `rume-app` — `Rume` builder, lifecycle, task runner, OpenAPI inspection, TLS/ACME, and module composition |
| `crates/grpc/` | `rume-grpc` — Tonic gRPC server, trace/context propagation, and `AppError` status mapping |
| `crates/graphql/` | `rume-graphql` — `async-graphql` Axum integration, trace extension, and `AppError` mapping |
| `crates/testing/` | `rume-testing` — test harnesses and fixtures |
| `crates/macro/` | `rume-macros` — procedural macros (`embed_migrations!`, `sql_assets!`, `#[derive(HasOpenApiSchema)]`) |
| `crates/cli/` | `rume-cli` — `rume` CLI (`new`, `doctor`, `dev`, `task`, `openapi`, `db`, `sql:check`, `list:routes`, `codegen`, `generate`, `make:migration`, `config`, `env`, `completions`) |
| `crates/crash/` | `rume-crash` — internal crash-recovery test harness (`publish = false`) |
| `crates/xtask/` | `xtask` — workspace automation tasks |
| `fuzz/` | `rume-fuzz` — standalone `cargo-fuzz` targets for untrusted-input parsers (`publish = false`) |
| `benchmarks/load/` | `rume-load-bench` — standalone Goose 80/20 load harness for `workbench/multi-instance` (`publish = false`) |
| `workbench/simple-api/` | Simple API reference app |
| `workbench/modular-monolith/` | Modular monolith reference app |
| `workbench/multi-instance/` | Multi-instance reference app + Docker Compose qualification suite |
| `docs/specs/vision/README.md` | Product vision & architectural principles |
| `docs/specs/vision/ROADMAP.md` | Phased engineering roadmap |

## Verification & testing

Requires **Rust 1.96+** (Edition 2024).

```bash
# Format + strict Clippy across all workspace targets and features
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Unit, integration, and doc tests
cargo test --workspace --lib --all-features
cargo test -p rume --test integration --all-features
cargo test --workspace --doc --all-features

# Feature-isolated checks (matches CI backend isolation matrix)
for feat in "sqlite,testing" "postgres,testing" "mysql,testing" "redis,testing"; do
  cargo clippy -p rume --all-targets --no-default-features --features "$feat" -- -D warnings
done

# Hot-path Criterion micro-benchmarks
cargo bench -p rume --bench hot_path

# Continuous fuzzing (requires nightly + cargo-fuzz)
cargo +nightly fuzz run saml_parser -- -max_total_time=30
```

CI (`.github/workflows/ci.yml`) runs `rustfmt`, `clippy` (`-D warnings`), `doc & doctests`, `cargo audit`, MSRV (`1.96`), all-features and feature-isolated backend test matrices (`sqlite`, `postgres`, `mysql`, `redis`), `rume-cli`, `rume-macros`, all three `workbench` apps, and a Docker Compose multi-instance qualification suite. CodeQL runs via `.github/workflows/codeql.yml`.

## Benchmarks & load testing

See [BENCHMARKS.md](BENCHMARKS.md) for measured Criterion hot-path numbers (`raw_axum_router` vs. `ObservabilityLayer`, `CircuitBreaker`, `RateLimiter`, `TenantScopedCache`, 64-task singleflight contention), the `workbench` multi-instance system baseline, and the Goose 80/20 load-testing harness.

## License

MIT OR Apache-2.0
