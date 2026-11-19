# Plan ledger — plan: docs/plans/003-audit-remediation-and-hardening/

Ruling: Execute Plan 003 on branch `fix/audit-remediation-003` (base `main` `31683f6`) in 3 parallel disjoint waves, reusing persistent implementer subagents per user instruction.
Ruling: Skip all review subagents per explicit user instruction ("dont use review subagents no need to do review").
Ruling: Commit locally per wave/milestone on `fix/audit-remediation-003`, then push all commits and open a new PR targeting `main`.
Ruling: Upgrade Tasks 01, 02, 03, and 05 to use `tokio::task::JoinSet`, direct MySQL session execution, `sqlparser = "=0.53.0"`, and `chrono = "=0.4.45"`.
Ruling: Enforce `rules/70-code-rationale.md` — code + explicit `Why:` comments stand alone as single source of truth.
Ruling: Keep all Cargo check/clippy/test commands scoped with `-j 2` per local CPU constraint.

## Pre-flight scan

| Scope / Wave | Tasks | Disjoint Ownership | Notes |
|---|---|---|---|
| Step 0 | Prep | `.coderabbit.yaml`, Cargo pins, `docs/plans/003-...`, `#![forbid(unsafe_code)]` in `macro`/`xtask` | Clean base before Wave 1 |
| Wave 1 (parallel) | `01`, `02`, `04`, `07` | `typed.rs`+`worker.rs` vs `migration.rs`+`core_down.sql` vs `outbox/*`+`jobs/postgres.rs` vs `builder.rs`+`ext.rs` | Zero overlapping files; Task 02 updates integration/workbench migration len tests |
| Wave 2 (parallel) | `03`, `05`, `06` | `persistence/{row,backend,pg,my,sq}.rs` vs `foundation`+`saml`+`auth/redis`+`cli/db` vs `tenant/{cache,quota}.rs`+`typed.rs` (`default_ttl`) | `cache/typed.rs` is disjoint across Wave 1 -> Wave 2; note `core/src/lib.rs` only touched by Task 05 (deleting `safe_equal_hashed`) |
| Wave 3a (parallel) | `08`, `09` | `observability/*`+`events/bus.rs`+`saga/coordinator.rs`+`core/src/lib.rs` vs `crates/crash-harness`+SQL boundary moves | Zero overlapping files |
| Wave 3b | `10` | `testing/fixtures.rs` + `proptest` suites + `rume-cli` templates/CLI `--dialect` | Final task after all workspace refactoring lands |
