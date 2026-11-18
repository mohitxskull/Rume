# 14-rc09-metrics.md

## Goal

Fix RC-09 Prometheus metric correctness defects:

1. `crates/core/src/metrics/layer.rs` line 87: `req.method().to_string()` records the raw HTTP method as a label value. Arbitrary/malformed methods (e.g. from a fuzzed or extended CONNECT) cause unbounded label cardinality. Fix: normalize/validate the method to a known set (or a bounded "OTHER" bucket) before using it as a label.
2. `crates/core/src/metrics/registry.rs` lines 271-275: `method`, `status`, `pool`, and `check name` are interpolated raw into Prometheus text format (`http_requests_total{method="{method}"...}`). Unescaped backslash, double-quote, or newline in a label value breaks the Prometheus exposition format and can inject labels. Fix: escape label values per Prometheus rules (backslash → `\\`, double-quote → `\"`, newline → `\n`) at render time.

## Files touched

- `crates/core/src/metrics/layer.rs` — method label normalization (~line 87).
- `crates/core/src/metrics/registry.rs` — `render_prometheus` label escaping (~lines 271-314 and all other label interpolations).

## Constraints honoured

- Root cause, not symptom: bound cardinality at the label source; escape at the render sink.
- No new dependencies, no `unsafe`.
- Escaping applied consistently to every label interpolation in `render_prometheus` (method, status, pool, name, le).
- Record `Why:` comments for cardinality bounding and label escaping.

## Done when (checkable by a command)

- `cargo test -p rume-core metrics` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- Rendered output for a method containing `"` or `\` produces valid, parseable Prometheus text (escaped).
- Method labels are bounded to a known set + OTHER.

## Verification (fails-before test idea)

Tests:
1. **Cardinality:** record requests with methods `GET`, `GET`, and a fuzzed method `"GET\x00CUSTOM"` / an extended method; assert the number of distinct method labels stays bounded (e.g. known methods tracked individually, everything else in `OTHER`). Before the fix each distinct raw method is a new label → cardinality unbounded → test fails.
2. **Escaping:** render a registry with a label value containing a double-quote and backslash (e.g. method `GET"\\`); assert the output contains `method=\"GET\"\\\\` (properly escaped) and the output is parseable as Prometheus text. Before the fix raw `"` breaks the format → test fails.
