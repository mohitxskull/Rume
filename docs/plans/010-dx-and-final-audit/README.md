# Plan 010 — Milestone F: CLI, Developer Experience & Final Audit

## Goal

Close Milestone F ("CLI, Developer Experience & Final Audit") — adding a first-class application task runner (`ApplicationMode::Task` + `rume task`), OpenAPI v3.1 export and TypeScript SDK codegen (`ApplicationMode::OpenApi` + `rume openapi`), a live-reload development server (`rume dev`), and a final `README.md` sync, end-to-end test, and full workspace audit — executed **strictly sequentially on `main`**:

1. **Task Runner Mode & CLI (`rume-foundation`, `rume-app`, `rume-cli`, `rume`)** — Add `ApplicationMode::Task`, `TaskRegistry` / `TaskHandler`, `RumeBuilder::task(name, description, handler)`, and `rume task list` / `rume task run <name>` running with full `AppContext` (config, DB, cache, observability) and clean lifecycle shutdown.
2. **OpenAPI Export & TypeScript SDK Codegen (`rume-foundation`, `rume-app`, `rume-cli`)** — Add `ApplicationMode::OpenApi`, `RumeBuilder` OpenAPI JSON inspection (`RUME_INSPECT=openapi` / `--rume-inspect-openapi`), `rume openapi export`, and a pure-Rust OpenAPI v3.1 -> TypeScript interface + `fetch` client generator (`rume openapi ts`).
3. **`rume dev` Live-Reload Server (`rume-cli`)** — Implement `rume dev` (`--bin`, `--watch`, `--no-migrate`, `--debounce-ms`) with debounced polling file watching, `target/` and `.git/` ignore filters, optional pre-boot `db migrate`, and clean child-process restart/shutdown.
4. **`README.md` Sync, Facade & Final Workspace Audit (`README.md`, `crates/core`, workspace)** — Sync `README.md` with all shipped Milestones A–F capabilities, feature flags, workspace crates, and CLI commands; add `milestone_f_e2e.rs`; and pass full workspace verification (`fmt`, `cargo deny`, `clippy`, `--no-default-features`, `--lib`, `integration`, `--doc`).

## Current state (read before executing)

Facts verified against the tree at plan time:

- **Application mode (`crates/foundation/src/state.rs`)**:
  - `ApplicationMode` is `#[derive(Debug, Clone, Copy, PartialEq, Eq)]` with `Run` and `Inspect`.
- **Application builder (`crates/app/src/app/builder/mod.rs` & `router.rs`)**:
  - `RumeBuilder::effective_mode()` checks `RUME_INSPECT`, `--inspect`, and `--rume-inspect-routes`.
  - `RumeBuilder::run_inspect_mode()` validates modules, middleware, CORS, and routes, then prints `__RUME_ROUTES_JSON_START__` ... `__RUME_ROUTES_JSON_END__` when `RUME_INSPECT=routes` or `--rume-inspect-routes` is set.
  - `RumeBuilder::openapi(path, info)` in `crates/app/src/app/builder/router.rs` registers an OpenAPI JSON route using `rume_http::openapi::OpenApiInfo` and `RouteRegistry::to_openapi(info)`.
- **OpenAPI (`crates/http/src/openapi/spec.rs`)**:
  - `generate_openapi` produces OpenAPI `3.1.0` JSON (`OpenApiSpec`, `OpenApiPathItem`, `OpenApiOperation`, `OpenApiParameter`, `OpenApiSchema`, `OpenApiResponse`).
- **CLI (`crates/cli/Cargo.toml`, `crates/cli/src/cli.rs`, `crates/cli/src/commands/mod.rs`)**:
  - `rume-cli` depends on `rume` (`features = ["postgres", "sqlite", "mysql"]`), `clap`, `comfy-table`, `console`, `serde`, `serde_json`, `tokio`.
  - `list_routes.rs` spawns `cargo run --quiet [--bin <name>] -- --rume-inspect-routes` with `RUME_INSPECT=routes` and parses tagged JSON from stdout.
  - `commands/db/mod.rs` implements `migrate()`.
- **`README.md`**:
  - Still reflects pre-Milestone E feature flags and workspace layout (missing `crates/grpc`, `crates/graphql`, `tls`, `acme`, `static-embed`, `mailer`, `scim`, `grpc`, `graphql`, `s3`, `http-client`, `dns-srv`, `nats`, `rabbitmq`, and Milestone F CLI commands).

## Design Rulings

Autonomous engineering rulings for Milestone F (no open questions):

1. **Keep `ApplicationMode` `Copy` with unit variants `Task` and `OpenApi`:** `ApplicationMode` (`crates/foundation/src/state.rs`) is a Tier 1 `Copy` enum re-exported at `rume::ApplicationMode`. Adding unit variants `ApplicationMode::Task` and `ApplicationMode::OpenApi` (while passing the selected task name via `RumeBuilder::task_name(...)`, `RUME_TASK=<name>`, or `--rume-task <name>`) preserves `Copy` and avoids breaking existing callers.
2. **Reuse tagged stdout protocol for `rume task list` and `rume openapi export`:** Mirror `crates/cli/src/commands/list_routes.rs` (`__RUME_TASKS_JSON_START__`/`__RUME_TASKS_JSON_END__` and `__RUME_OPENAPI_JSON_START__`/`__RUME_OPENAPI_JSON_END__`) so CLI inspection works with any `Rume` binary via `cargo run --quiet` without extra IPC sockets or ports.
3. **Pure-Rust OpenAPI v3.1 -> TypeScript generator in `crates/cli`:** Implement the TypeScript generator directly over `serde_json::Value` in `crates/cli/src/commands/openapi/ts.rs` rather than adding external codegen dependencies. Supporting `--input <file.json>` in addition to live binary inspection makes `rume openapi ts` usable both against a live app and offline in CI.
4. **Pure-Rust debounced polling watcher for `rume dev`:** Use a `std::fs::metadata` recursive directory scanner (`modified()` + file length) with configurable `--debounce-ms` (default `300`) and `--watch` paths (defaulting to `src`, `config`, `migrations`, `sql`, `Cargo.toml`) ignoring `target/` and `.git/`, instead of adding `notify`. Zero new dependencies, zero `unsafe`, deterministic behavior across Linux/WSL/containers/NFS, and 100% hermetic unit testing.

## Global constraints

- **Workspace invariants**: `#![forbid(unsafe_code)]` in all library crates; Rust Edition 2024; MSRV `1.96`; concrete version pins (`=x.y.z`) in root `Cargo.toml`; all dependencies must pass `cargo deny check licenses` against `deny.toml`.
- **Strict 7-layer DAG**: L0 `rume-foundation` -> L1 `rume-observability` -> L2 `rume-data` -> L3 `rume-messaging`/`rume-security`/`rume-enterprise` -> L4 `rume-http` -> L5 `rume-app` -> L6 `rume` (`crates/core`). `rume-cli` sits above `rume`.
- **Build concurrency**: Every `cargo` command MUST pass `-j 2`.
- **Zero-warning bar** (`rules/80-quality-bar.md`): `cargo fmt --all --check` and `cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings` must pass with zero warnings.
- **Execution mode (strictly sequential on `main`)**: Execute tasks `01` through `04` strictly in order on `main`, one implementer subagent at a time (`rules/30-git.md`, `rules/50-agent-conduct.md`).
- **TDD & fails-before** (`rules/50-agent-conduct.md`, `rules/80-quality-bar.md`): Write tests first for each task, confirm fails-before, then implement minimal clean code.
- **Commit discipline** (`rules/30-git.md`): Conventional Commits; no commits unless explicitly asked or announced up front by the controller.

## Constraints honoured

- **Application mode & lifecycle**: `crates/foundation/src/state.rs` (`ApplicationMode`), `crates/app/src/app/builder/mod.rs` (`RumeBuilder::effective_mode`, `run_inspect_mode`, `run_with_signal`), `crates/app/src/app/lifecycle.rs` (`run_startup`, `run_shutdown`).
- **OpenAPI spec & builder**: `crates/http/src/openapi/spec.rs` (`OpenApiSpec`, `generate_openapi`), `crates/app/src/app/builder/router.rs` (`RumeBuilder::openapi`).
- **CLI structure & project guard**: `crates/cli/src/cli.rs`, `crates/cli/src/commands/mod.rs` (`require_rume_project`), `crates/cli/src/commands/list_routes.rs`, `crates/cli/src/commands/db/mod.rs`.
- **Rules**: `rules/10-tech-stack.md`, `rules/40-verification.md`, `rules/50-agent-conduct.md`, `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`, `rules/testing.md`.

## Task index

- `01-task-runner-mode-and-cli.md` — Add `ApplicationMode::Task`, `TaskRegistry` / `RumeBuilder::task(...)` lifecycle execution, and `rume task list` / `rume task run <name>` CLI commands.
- `02-openapi-export-and-ts-codegen.md` — Add `ApplicationMode::OpenApi`, `RumeBuilder` OpenAPI JSON export hook, `rume openapi export`, and `rume openapi ts` pure-Rust TypeScript SDK generator.
- `03-rume-dev-live-reload.md` — Implement `rume dev` (`--bin`, `--watch`, `--no-migrate`, `--debounce-ms`) with debounced polling watcher, ignore filters, pre-boot migration check, and clean process restart.
- `04-readme-sync-and-final-audit.md` — Sync `README.md` with Milestones A–F, add `milestone_f_e2e.rs`, and run full workspace verification.

## Done-when checklist

- [ ] `ApplicationMode::Task` and `RumeBuilder::task(...)` execute registered tasks with full `AppContext` and clean startup/shutdown without binding an HTTP listener.
- [ ] `rume task list` and `rume task run <name>` work against a Rume binary.
- [ ] `ApplicationMode::OpenApi` and `rume openapi export` emit valid OpenAPI v3.1 JSON; `rume openapi ts` generates typed TypeScript interfaces and `fetch` client methods.
- [ ] `rume dev` watches configured paths with debouncing, ignores `target/` and `.git/`, optionally runs `db migrate`, and cleanly restarts the child server on changes.
- [ ] `README.md` accurately documents all shipped features, feature flags, workspace crates, and CLI commands.
- [ ] `milestone_f_e2e.rs` and all workspace verification commands pass with zero warnings.

## Verification commands

```bash
cargo fmt --all --check
cargo deny check licenses
cargo clippy -j 2 --workspace --all-targets --all-features -- -D warnings
cargo check -j 2 -p rume --no-default-features
cargo test -j 2 --workspace --lib --all-features
cargo test -j 2 -p rume --test integration --all-features
cargo test -j 2 --workspace --doc --all-features
```
