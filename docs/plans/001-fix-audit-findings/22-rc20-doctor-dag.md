# 22-rc20-doctor-dag.md

## Goal

Fix RC-20: `crates/cli/src/commands/doctor.rs` `check_module_dag` uses a line-by-line DAG parser (`extract_module_name`/`extract_module_dependencies`, ~lines 471-512) that breaks on rustfmt multi-line formatting (e.g. a `dependencies(&self)` fn whose body spans multiple lines with the closing `}` on a later line, or quoted strings spread across lines). The parser stops at the first line containing `}` and scans quotes naively, so multi-line dependency lists are missed or mis-parsed.

Fix root cause: replace the line-by-line scan with a brace-aware scan that tracks `{`/`}` depth across lines to correctly delimit the `dependencies` function body, and extract all quoted module dependencies within that balanced region (handles rustfmt multi-line output).

## Files touched

- `crates/cli/src/commands/doctor.rs` — `extract_module_name`, `extract_module_dependencies`, `check_module_dag` (~lines 471-469 region).

## Constraints honoured

- Root cause, not symptom: parse the Rust structure (brace depth) rather than line heuristics.
- No new dependencies, no `unsafe`. (Do not pull in `syn` just for this — keep it a small brace-scanner.)
- Record a `Why:` comment explaining brace-depth tracking replaces line-by-line heuristics.

## Done when (checkable by a command)

- `cargo test -p rume-cli doctor` passes (DAG test).
- `cargo clippy -p rume-cli --all-targets` and `cargo fmt --all --check` clean.
- A `dependencies(&self)` fn formatted across multiple lines by rustfmt is parsed correctly: all deps extracted, no premature stop at an inner `}`.

## Verification (fails-before test idea)

Test with a fixture source string mimicking rustfmt multi-line output:

```rust
fn dependencies(&self) -> Vec<&str> {
    vec![
        "module_a",
        "module_b",
    ]
}
```

1. **Before fix:** line-by-line parser either stops early or misses `"module_b"` → test fails.
2. **After fix:** `extract_module_dependencies` returns `["module_a", "module_b"]` → test passes.

Also test single-line form still works.
