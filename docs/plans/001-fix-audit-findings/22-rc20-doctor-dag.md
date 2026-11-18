# 22-rc20-doctor-dag.md

## OVERRIDE — Dependency-based approach
Do NOT hand-roll a brace-depth scanner. Instead:
- Add the `syn` crate to workspace dependencies and cli Cargo.toml (syn is already a dependency of rume-macros — check workspace)
- Rewrite `crates/cli/src/commands/doctor.rs` to use `syn::parse_file` for parsing module declarations and dependencies
- `syn` handles multi-line rustfmt formatting natively — `fn name(&self) -> &str { "..." }` on multiple lines, quoted strings spread across lines, etc.

## Task file
Read the full task file: /home/skull/Projects/rume/docs/plans/001-fix-audit-findings/22-rc20-doctor-dag.md

## Your job
Fix RC-20 `rume doctor` DAG parser defects using syn:

1. Add `syn` crate to workspace `Cargo.toml` (`[workspace.dependencies]`) and `crates/cli/Cargo.toml` (syn already in workspace via rume-macros)
2. Replace hand-rolled line-by-line `extract_module_name`/`extract_module_dependencies` with `syn::parse_file` parsing
3. `syn` handles multi-line rustfmt formatting: `fn name(&self) -> &str { "..." }` across lines, quoted strings spread across lines
4. `check_module_dag` correctly detects modules and cycles in standard rustfmt-formatted code

## Files touched
- `Cargo.toml` (workspace deps)
- `crates/cli/Cargo.toml` (add syn)
- `crates/cli/src/commands/doctor.rs` — rewrite using syn::parse_file

## Constraints
- Use syn for Rust source parsing
- No `unsafe`
- Record `Why:` comments at decision sites
- Multi-line `fn name(&self) -> &str { "..." }` parsed correctly
- Multi-line `dependencies(&self)` vec parsed correctly

## Verification
1. `cargo test -p rume-cli doctor`
2. `cargo clippy -p rume-cli --all-targets`
3. `cargo fmt --all --check`

## Handoff
Write to `/home/skull/Projects/rume/docs/plans/001-fix-audit-findings/scratch/task-22-done.md`. Do not commit.
