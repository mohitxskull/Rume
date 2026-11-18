# 10-rc14-safe-equal.md

## OVERRIDE — Dependency-based approach
Do NOT hand-roll constant-time comparison. Instead:
- Add the `subtle` crate to workspace dependencies and core Cargo.toml
- Rewrite `crates/core/src/helpers/safe_equal.rs` to use `subtle::ConstantTimeEq`
- `subtle::ConstantTimeEq` is audited crypto code — no length-dependent loop, no timing side-channel from loop bounds

## Task file
Read the full task file: /home/skull/Projects/rume/docs/plans/001-fix-audit-findings/10-rc14-safe-equal.md

## Your job
Fix RC-14 `safe_equal` length-dependent timing leak using `subtle`:

1. Add `subtle` crate to workspace `Cargo.toml` (`[workspace.dependencies]`) and `crates/core/Cargo.toml` (pin exact version, e.g. `=2.6.1`)
2. Rewrite `crates/core/src/helpers/safe_equal.rs` to use `subtle::ConstantTimeEq::ct_eq()` instead of hand-rolled `max_len` loop with SHA-256 hashing
3. Preserve public API: `safe_equal` and `safe_equal_hashed`
4. `subtle::ConstantTimeEq` is audited, constant-time, length-independent

## Files touched
- `Cargo.toml` (workspace deps)
- `crates/core/Cargo.toml` (add subtle)
- `crates/core/src/helpers/safe_equal.rs` — rewrite to use subtle::ConstantTimeEq

## Constraints
- Use `subtle::ConstantTimeEq` for constant-time comparison
- No `unsafe`
- Record `Why:` comments at decision sites
- `safe_equal("a", "aaaaaaaaaa")` and `safe_equal("a", "a")` take constant time regardless of length difference
- Existing API preserved (`safe_equal`, `safe_equal_hashed`)

## Verification
1. `cargo test -p rume helpers`
2. `cargo clippy -p rume-core --all-targets`
3. `cargo fmt --all --check`

## Handoff
Write to `/home/skull/Projects/rume/docs/plans/001-fix-audit-findings/scratch/task-10-done.md`. Do not commit.
