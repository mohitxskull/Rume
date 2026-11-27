# Task 01 — `ApplicationMode::Task` & `rume task` Runner

## Goal

Support one-off application tasks that boot with full `AppContext` (config, DB, cache, observability, module startup/shutdown) without binding an HTTP listener, exposed via `RumeBuilder::task(...)`, `ApplicationMode::Task`, and `rume task list` / `rume task run <name>`.

## Files touched

- `crates/foundation/src/state.rs`
- `crates/app/src/app/task.rs`, `crates/app/src/app/mod.rs`, `crates/app/src/app/builder/mod.rs`
- `crates/core/src/lib.rs`, `crates/core/src/prelude.rs`
- `crates/cli/src/cli.rs`, `crates/cli/src/commands/mod.rs`, `crates/cli/src/commands/task.rs`

## Deliverable

- `ApplicationMode::Task` added to `crates/foundation/src/state.rs` (preserving `Copy`).
- `crates/app/src/app/task.rs` providing `TaskInfo` (`name`, `description`), `TaskHandler` (async closure/fn receiving `AppContext<E, C, S>` and returning `Result<(), Box<dyn std::error::Error + Send + Sync>>`), and `TaskRegistry` (preserving registration order, rejecting duplicate names or validating lookup).
- `RumeBuilder` methods:
  - `task(name, description, handler)` — registers a named task.
  - `task_name(name)` — sets the task to execute when `mode` is `ApplicationMode::Task`.
  - `effective_mode()` recognizes `RUME_TASK`, `RUME_TASK_LIST`, `--rume-task`, and `--rume-list-tasks` as `ApplicationMode::Task`.
  - `run_task_mode()` runs `resolve_modules`, `build_router_with_resolved` (or route/context assembly), `run_startup`, then either prints `__RUME_TASKS_JSON_START__\n...\n__RUME_TASKS_JSON_END__` (when listing) or runs the selected task with `serve_ctx`, and always executes `run_shutdown` cleanly before returning.
- `rume` (`crates/core`) re-exports `TaskInfo`, `TaskRegistry` (and `TaskHandler` if public) in `rume::app` / `rume::prelude`.
- `crates/cli/src/cli.rs` and `crates/cli/src/commands/task.rs` implementing:
  - `rume task list [--bin <name>] [--format <human|json>] [--json]`
  - `rume task run <name> [--bin <name>]`

## Constraints honoured

- `crates/foundation/src/state.rs` (`ApplicationMode` remains `Copy`).
- `crates/app/src/app/builder/mod.rs` (`RumeBuilder::effective_mode`, `run_with_signal`, `run_with_listener_and_signal`).
- `crates/app/src/app/lifecycle.rs` (`resolve_modules`, `run_startup`, `run_shutdown`).
- `crates/cli/src/commands/list_routes.rs` (tagged stdout JSON pattern for CLI inspection).
- `rules/50-agent-conduct.md` (TDD), `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`.

## Named tests (bodies omitted)

- `app::task::tests::task_registry_lists_registered_tasks_in_order` — registers two tasks and verifies `TaskRegistry::list()` returns `TaskInfo` entries in insertion order.
- `app::builder::tests::task_mode_runs_selected_task_with_startup_and_shutdown` — builds a `RumeBuilder` with a module lifecycle tracker and a task reading `AppContext`, runs in `ApplicationMode::Task` with `.task_name(...)`, and asserts `on_start`, task execution, and `on_shutdown` all ran without binding a port.
- `app::builder::tests::task_mode_unknown_task_returns_not_found_after_shutdown` — runs `ApplicationMode::Task` with an unknown task name and asserts `Err` (`ErrorKind::NotFound`) and clean `on_shutdown`.
- `commands::task::tests::parse_tasks_json_extracts_tagged_payload` — parses tagged `__RUME_TASKS_JSON_START__` ... `__RUME_TASKS_JSON_END__` stdout into `Vec<TaskInfo>` and errors on missing tags.

## Verification

```bash
cargo fmt --all --check && \
cargo clippy -j 2 -p rume-foundation -p rume-app -p rume -p rume-cli --all-targets --all-features -- -D warnings && \
cargo test -j 2 -p rume-foundation -p rume-app -p rume-cli --lib --all-features
```
Expected: all commands exit 0 with zero warnings and all tests passing.

## Commit

Only when the user asks:
```text
feat(app,cli): add ApplicationMode::Task, TaskRegistry, and `rume task` CLI
```

## Done-when

- [ ] `ApplicationMode::Task` exists and `ApplicationMode` remains `Copy`.
- [ ] `RumeBuilder::task(...)` and `RumeBuilder::task_name(...)` execute tasks with full `AppContext` and clean startup/shutdown.
- [ ] `rume task list` and `rume task run <name>` are wired in `rume-cli` and tested.
- [ ] Verification command passes cleanly.
