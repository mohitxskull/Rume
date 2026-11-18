# 11-rc03-env-loading.md

## Goal

Fix RC-03 environment-loading defects in `crates/core/src/env/loading.rs`:

1. Figment Value coercion (~lines 114-121): `value.parse::<figment::value::Value>()` lets figment coerce raw strings (e.g. `"true"` → bool, `"123"` → int, `"0x10"` → int), silently changing the type of environment values before they reach typed config. Fix: preserve values as literal strings using `figment::value::Value::from(value)` instead of `parse::<Value>()`.
2. Secret classification ignores value content (line 214): values are routed to `secrets` vs `values` based only on `is_secret_key(key)`, ignoring `value_looks_sensitive(value)`. A sensitive value under a non-secret key (e.g. `DATABASE_URL=postgres://user:pass@host` under key `DATABASE_URL`) stays in plaintext `values`. Fix: route to `secrets` when `is_secret_key(key) || value_looks_sensitive(&interpolated)`.

## Files touched

- `crates/core/src/env/loading.rs` — `load_from_resolved` (Value coercion, lines 103-140) and `load_merged_map_for_env_in` secret routing (line 214).

## Constraints honoured

- Root cause, not symptom: stop coercing at the boundary and classify secrets by value as well as key.
- No new dependencies (`figment`, `secrecy`, `value_looks_sensitive` already present).
- `value_looks_sensitive` already exists in `crate::env::secrets` (re-exported via `env::mod`).
- Record `Why:` comments: raw-string preservation prevents type surprises; value-based classification prevents plaintext leakage of sensitive values under non-secret keys.

## Done when (checkable by a command)

- `cargo test -p rume-core env` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- A value like `"true"` under a non-secret key loads as the string `"true"` (not boolean coercion).
- A sensitive value under a non-secret key is present in `resolved.secrets`, not `resolved.values`.

## Verification (fails-before test idea)

Tests:
1. **No coercion:** load an env with `FLAG=true` and `COUNT=123` under non-secret keys; assert `resolved.values.get("FLAG") == Some("true")` and `resolved.values.get("COUNT") == Some("123")` (literal strings, not coerced). Before the fix figment coerces → test fails.
2. **Value-based secret routing:** load `DATABASE_URL=postgres://user:pass@db:5432/app` under a non-secret key; assert it appears in `resolved.secrets` (because `value_looks_sensitive` detects the credential URL) and not in plaintext `values`. Before the fix it's in `values` → test fails.
