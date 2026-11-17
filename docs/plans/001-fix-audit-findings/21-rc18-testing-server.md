# 21-rc18-testing-server.md

## Goal

Fix RC-18 testing/server.rs defects in `crates/core/src/testing/server.rs`:

1. `header.insert` overwrite: `from_reqwest_response` (~lines 184-190) uses `headers.insert(hname, hval)`, which overwrites duplicate headers instead of appending them. Multi-value headers (e.g. multiple `Set-Cookie`) lose values. Fix: use `headers.append`.
2. Weak assertions: the `test_server_ephemeral_tcp_lifecycle_and_requests` test only asserts status/body, not header round-tripping or multi-value headers — the `header.insert` defect is untested. Fix: strengthen the test to assert multi-value headers survive.
3. Wall-clock sleeps: the test uses real `tokio::time::sleep`-based timing indirectly via the server (shutdown timeout). Fix: keep sleeps bounded and deterministic; prefer `assert` over `sleep`-based waits where possible.

Fix root cause: switch `insert` → `append` for headers; add a multi-value header assertion to the test; keep any sleeps bounded/deterministic.

## Files touched

- `crates/core/src/testing/server.rs` — `from_reqwest_response` header copy (~lines 184-190); the test at ~lines 249-295.

## Constraints honoured

- Root cause, not symptom: duplicate headers are appended, not overwritten.
- No new dependencies, no `unsafe`.
- Record a `Why:` comment explaining `append` preserves multi-value headers.

## Done when (checkable by a command)

- `cargo test -p rume-core testing::server` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- A response with two `Set-Cookie` headers returns both (asserted in the test).

## Verification (fails-before test idea)

Test:
1. Serve a handler that returns two `Set-Cookie` headers (e.g. `Set-Cookie: a=1` and `Set-Cookie: b=2`) plus a duplicate of another header.
2. **Before fix** `headers.insert` keeps only the last value → `TestResponse` has only one `Set-Cookie` → test fails; **after fix** `append` preserves both → test passes.

Assert `resp.headers().get_all("set-cookie").len() == 2`.
