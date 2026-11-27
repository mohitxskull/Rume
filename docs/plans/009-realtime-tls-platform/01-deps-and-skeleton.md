# Task 01 — Dependencies, Feature Wiring, SQL Assets & Compiling Skeleton

## Goal

Pin all Milestone E crates in root `Cargo.toml`, add `crates/grpc` (`rume-grpc`) and `crates/graphql` (`rume-graphql`) to the workspace, wire Cargo features and 7-layer DAG edges cleanly, add SQLite/Postgres/MySQL SQL assets in `crates/data/sql/` + `crates/data/src/sql.rs`, and scaffold compiling `todo!()` skeletons for all Milestone E modules so subsequent tasks run TDD fails-before against real types.

## Files touched

- `Cargo.toml` (workspace members + `[workspace.dependencies]` pins)
- `crates/foundation/src/env/secret_provider.rs`, `crates/foundation/src/env/mod.rs`
- `crates/data/sql/{sqlite,postgres,mysql}/{feature_flags,usage_metering,webhook_deliveries}_schema.sql` (and DML files), `crates/data/src/sql.rs`
- `crates/security/Cargo.toml`, `crates/security/src/flags/{mod.rs,...}`, `crates/security/src/metering/{mod.rs,...}`, `crates/security/src/lib.rs`
- `crates/messaging/Cargo.toml`, `crates/messaging/src/webhooks/{mod.rs,...}`, `crates/messaging/src/mailer/{mod.rs,...}`, `crates/messaging/src/lib.rs`
- `crates/enterprise/src/enterprise/scim/{mod.rs,filter.rs,store.rs}` (or submodules under `crates/enterprise/src/enterprise/scim.rs`)
- `crates/http/Cargo.toml`, `crates/http/src/{tls,realtime,static_files,http_cache,webhooks,scim}/mod.rs`, `crates/http/src/routing/versioning.rs`, `crates/http/src/lib.rs`
- `crates/app/Cargo.toml`, `crates/app/src/app/builder/mod.rs`
- `crates/testing/src/mailer.rs`, `crates/testing/src/lib.rs`
- `crates/grpc/Cargo.toml`, `crates/grpc/src/lib.rs`
- `crates/graphql/Cargo.toml`, `crates/graphql/src/lib.rs`
- `crates/core/Cargo.toml`

## Deliverable

- All Milestone E dependencies pinned with exact `=x.y.z` versions in root `Cargo.toml`:
  `tokio-rustls = "=0.26.6"`, `rustls = "=0.23.46"`, `rustls-pki-types = "=1.16.0"`, `rustls-pemfile = "=2.2.0"`, `hyper-util = "=0.1.21"`, `rustls-acme = "=0.15.4"`, `rust-embed = "=8.14.0"`, `lettre = { version = "=0.11.24", default-features = false, features = ["tokio1-rustls-tls", "smtp-transport", "builder", "hostname", "pool"] }`, `minijinja = "=3.1.0"`, `tonic = "=0.14.9"`, `prost = "=0.14.6"`, `tonic-prost = "=0.14.9"`, `async-graphql = "=7.2.1"`, `async-graphql-axum = "=7.2.1"`.
- `axum` in `crates/http/Cargo.toml` enables `"ws"`, and `tower-http` enables `"fs"`.
- `crates/security/Cargo.toml` adds optional `sqlx` + `sqlite`, `postgres`, `mysql` features (matching `rume-messaging`/`rume-enterprise`), and `crates/core/Cargo.toml` forwards `rume-security/{sqlite,postgres,mysql}` in its `sqlite`, `postgres`, and `mysql` features.
- Feature flags wired cleanly:
  - `tls`, `acme` (implies `tls`), `static-embed` on `rume-http`, `rume-app`, `rume`;
  - `mailer` on `rume-messaging`, `rume`;
  - `scim` on `rume-http` (gating `rume-http::scim` and optional `dep:rume-enterprise`) and `rume`;
  - `grpc`, `graphql` on `rume` (gating optional `rume-grpc` / `rume-graphql`).
- New SQL schema & query constants exported from `crates/data/src/sql.rs` for `rume_feature_flags`, `rume_usage_counters`, and `rume_webhook_deliveries` across SQLite, Postgres, and MySQL.
- Compiling `todo!()` skeletons (with `#![forbid(unsafe_code)]`) for every Milestone E module and crate.

## Constraints honoured

- `rules/10-tech-stack.md` (concrete `=x.y.z` pins).
- `deny.toml` (all licenses in allow-list; run `cargo deny check licenses`).
- `crates/data/src/sql.rs` (centralized SQL assets, no cross-crate `include_str!`).
- Strict 7-layer DAG: `rume-http` (L4) -> `rume-messaging`/`rume-enterprise`/`rume-security` (L3) -> `rume-data` (L2).
- Existing workspace tests continue to compile and pass.

## Named tests (bodies omitted)

- `rume_data::sql::tests::milestone_e_sql_assets_are_non_empty` — asserts all new SQLite/Postgres/MySQL DDL/DML constants for feature flags, usage metering, and webhook deliveries are non-empty.

## Verification

```bash
cargo fmt --all --check && cargo deny check licenses && cargo check -j 2 --workspace --all-targets --all-features
```
Expected: exit code 0, zero errors, `cargo deny check licenses` passes.

## Commit

Only when the user asks:
```text
chore: scaffold Milestone E dependencies, features, SQL assets, and skeletons
```

## Done-when

- [ ] Root `Cargo.toml` pins all Milestone E crates with `=x.y.z` and includes `crates/grpc` and `crates/graphql`.
- [ ] `cargo deny check licenses` passes cleanly.
- [ ] `cargo check -j 2 --workspace --all-targets --all-features` passes with zero errors.
