# Task 03 — `rume dev` Live-Reload Server

## Goal

Implement `rume dev` in `crates/cli` to run a debounced watch-and-rebuild development loop that ignores `target/` and `.git/`, optionally runs `db migrate` before initial boot, and cleanly restarts the child server process on file changes.

## Files touched

- `crates/cli/src/cli.rs`
- `crates/cli/src/commands/mod.rs`
- `crates/cli/src/commands/dev.rs`

## Deliverable

- `Commands::Dev` in `crates/cli/src/cli.rs` with flags:
  - `--bin <name>`: Optional binary name for multi-binary projects.
  - `--watch <path>`: Repeatable watch path (defaults to `src`, `config`, `migrations`, `sql`, `Cargo.toml` — filtered to existing paths).
  - `--no-migrate`: Skip running `db migrate` before initial server boot.
  - `--debounce-ms <u64>`: Debounce window in milliseconds (default `300`).
- `crates/cli/src/commands/dev.rs`:
  - `should_ignore_path(path)` ignoring `target`, `.git`, `.hg`, `.svn`, and editor swap/temp files (`*.swp`, `*~`).
  - `snapshot_tree(roots)` collecting a deterministic `BTreeMap<PathBuf, (u64, SystemTime)>` of file sizes and modification times.
  - `diff_snapshots(prev, next)` returning changed/added/removed paths.
  - `Debouncer` coalescing rapid change bursts within `--debounce-ms` before triggering a rebuild.
  - `build_cargo_run_args(bin)` constructing `["run"]` or `["run", "--bin", <name>]`.
  - Pre-boot migration step: unless `--no-migrate` is set, if `migrations/` exists, invokes `crate::commands::db::migrate().await` before spawning the first `cargo run`.
  - Process lifecycle loop: spawns `cargo run`, monitors file changes and `tokio::signal::ctrl_c()`, cleanly kills and awaits the child process before respawning on rebuild or exiting on `Ctrl-C`.

## Constraints honoured

- `crates/cli/src/commands/mod.rs` (`require_rume_project`).
- `crates/cli/src/commands/db/mod.rs` (`migrate`).
- `rules/50-agent-conduct.md` (TDD), `rules/70-code-rationale.md`, `rules/80-quality-bar.md`, `rules/85-minimalism.md`.

## Named tests (bodies omitted)

- `commands::dev::tests::ignore_filter_skips_target_git_and_swap_files` — asserts `target/...`, `.git/...`, and `.swp` paths are ignored while `src/main.rs`, `config/app.toml`, and `migrations/001.up.sql` are watched.
- `commands::dev::tests::snapshot_diff_detects_create_modify_and_delete` — uses `tempfile::TempDir` to create, modify, and delete files and asserts `diff_snapshots` detects each change while ignoring `target/` inside the temp dir.
- `commands::dev::tests::debouncer_coalesces_events_within_window` — feeds multiple change timestamps within `300ms` and asserts `Debouncer` fires once only after the quiet window elapses.
- `commands::dev::tests::build_cargo_run_args_handles_default_and_named_bin` — verifies `None` -> `["run"]` and `Some("api")` -> `["run", "--bin", "api"]`.

## Verification

```bash
cargo fmt --all --check && \
cargo clippy -j 2 -p rume-cli --all-targets --all-features -- -D warnings && \
cargo test -j 2 -p rume-cli --lib --all-features
```
Expected: all commands exit 0 with zero warnings and all tests passing.

## Commit

Only when the user asks:
```text
feat(cli): add `rume dev` debounced live-reload server command
```

## Done-when

- [ ] `rume dev` is wired in `crates/cli/src/cli.rs` and `crates/cli/src/commands/mod.rs`.
- [ ] Ignore filters, recursive snapshot diffing, debouncer, and `cargo run` arg construction have passing hermetic unit tests.
- [ ] Pre-boot `db migrate` runs when `migrations/` exists unless `--no-migrate` is passed, and child processes are cleanly terminated on restart and `Ctrl-C`.
- [ ] Verification command passes cleanly.
