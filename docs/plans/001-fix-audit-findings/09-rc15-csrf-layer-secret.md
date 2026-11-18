# 09-rc15-csrf-layer-secret.md

## Goal

Fix RC-15: `CsrfLayer` secret handling contradictions and the single-`Cookie`-header check.

1. `CsrfLayer::new()` currently auto-generates a random secret, while `#[derive(Default)]` produces a secret-less layer that fails closed — the two constructors contradict each other and the public API is ambiguous.
2. `auto_inject_cookie` (csrf.rs ~lines 302-315) inspects only a single `Cookie` header via `req.headers().get(COOKIE)`, missing tokens split across multiple `Cookie` headers (subdomain cookie-tossing DoS / injection bypass). Line 368 already uses `get_all(COOKIE)` for validation; injection must match.

Fix root cause (per scope decisions):
- Change `CsrfLayer::new()` to require an explicit secret: `CsrfLayer::new(secret: impl AsRef<[u8]>)`.
- Add `CsrfLayer::with_random_secret()` as the explicit opt-in constructor that auto-generates a secret.
- Remove `#[derive(Default)]` from `CsrfLayer` (a secret-less layer must not be silently constructible).
- Fix `auto_inject_cookie` to use `req.headers().get_all(COOKIE)` like the validation path at line 368.

## Files touched

- `crates/core/src/security/csrf.rs` — `CsrfLayer::new`, `CsrfLayer::with_random_secret`, `#[derive(Default)]`, `auto_inject_cookie` cookie inspection.

## Constraints honoured

- `CsrfLayer::new(secret: impl AsRef<[u8]>)` requires an explicit secret.
- `CsrfLayer::with_random_secret()` is the explicit opt-in for a generated secret.
- Remove `#[derive(Default)]` (or make Default fail-closed with no secret — removal chosen per scope).
- `auto_inject_cookie` uses `get_all(COOKIE)` (all Cookie headers).
- No new dependencies, no `unsafe`.
- Record a `Why:` comment explaining secret-less construction is removed to fail closed.

## Done when (checkable by a command)

- `cargo test -p rume-core security` passes (csrf tests).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `CsrfLayer::new("secret")` compiles; `CsrfLayer::new()` (no arg) no longer compiles; `CsrfLayer::with_random_secret()` compiles; `#[derive(Default)]` removed.
- `auto_inject_cookie` iterates `req.headers().get_all(COOKIE)`.

## Verification (fails-before test idea)

Tests:
1. **Constructor:** `CsrfLayer::new(b"s3cr3t")` produces a layer that verifies HMAC-signed tokens; `CsrfLayer::new()` (no args) is a compile error — assert via a compile-fail check or simply that the no-arg call no longer type-checks (the test file itself won't compile before the fix, which is the fails-before signal).
2. **Multiple Cookie headers:** a safe GET with `auto_inject_cookie` and a valid csrf token split across two `Cookie` headers (e.g. `Cookie: csrf_token=otherjunk` and `Cookie: csrf_token=<valid>`) is recognized as having a valid cookie → no re-injection. Before the fix only the first header is read → if the valid token is in the second header, it's missed → test fails.
3. **No Default:** `CsrfLayer::default()` no longer exists (compile error before fix if referenced).
