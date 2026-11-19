# Task 05 — Security Path Canonicalization, SAML Hardening, Redis Session JSON & Helper Cleanup (`RC-04`)

## Goal

Fail closed on `fs::canonicalize` errors when resolving `file:` secrets, replace hand-rolled SAML timestamp/offset parsing with `chrono::DateTime::parse_from_rfc3339`, harden SAML assertion signature-reference binding, `<saml:Issuer>` extraction, and `saml-rs` documentation, replace Redis session Lua string hacking with structural `cjson` mutation, and delete unused `safe_equal_hashed`.

## Files touched

- `Cargo.toml` (`[workspace.dependencies]`: add `chrono = "=0.4.45"`, update lines 64–67 `saml-rs` comment)
- `crates/enterprise/Cargo.toml` (add optional `chrono = { workspace = true, optional = true }` and enable `"dep:chrono"` in `saml` feature)
- `crates/foundation/src/env/loading.rs` (`resolve_value`)
- `crates/cli/src/commands/db/mod.rs` (`resolve_file_value`)
- `crates/enterprise/src/enterprise/saml.rs` (`parse_iso8601_strict` via `chrono`, remove `parse_tz_offset`/manual calendar math, `SamlParser::parse_internal`, `XmlNs`, module docs)
- `crates/core/tests/integration/enterprise.rs` (`test_saml_signature_verifier`, `test_saml_trust_boundary_exact_assertion_verified`, plus new negative tests)
- `crates/security/src/auth/redis.rs` (`REGENERATE_ID_SCRIPT`, `regenerate_id`, remove `escape_lua_pattern` / `escape_lua_replacement`, update `#[cfg(test)]`)
- `crates/security/src/auth/session.rs` (`Session` serde defaults for empty Lua table roundtrip)
- `crates/core/tests/containers/redis.rs` (`test_redis_session_store_save_load_ttl_and_destroy` at lines 646–685)
- `crates/foundation/src/helpers/safe_equal.rs` & `crates/foundation/src/helpers/mod.rs` (delete `safe_equal_hashed`)

## Deliverable

1. **Fail-closed `file:` canonicalization (`loading.rs:367-382`, `db/mod.rs:361-370`):**
   - Propagate errors from `fs::canonicalize(dir)` and `fs::canonicalize(&full)` (`?` / `.map_err(...)`) instead of falling back to uncanonicalized paths, check `canonical_full.starts_with(&canonical_dir)`, and read via `fs::read_to_string(&canonical_full)` rather than `&full`.
2. **SAML hardening (`crates/enterprise/src/enterprise/saml.rs`, `OQ-02` settled):**
   - In `signed_mode` (`verifier.is_some()`), require `!signature_references.is_empty()`, require `assertion_id` to be `Some(id)` where `!id.trim().is_empty()`, and require at least one `<ds:Reference>` to match `assertion_id` (keeping `URI=""` rejected at `:590-596` and preserving `parse_unverified` leniency for Response-level envelopes in `enterprise.rs:278,537`). Update existing signed-mode fixtures in `enterprise.rs:153-171,473-494` to carry `<ds:Signature><ds:SignedInfo><ds:Reference URI="#..." /></ds:SignedInfo></ds:Signature>`.
   - Anchor `<saml:Issuer>` extraction outside `if in_assertion`: extend `element_stack` (`saml.rs:407,490`) from `Vec<(String, bool)>` to `Vec<(String, XmlNs)>` using a module-private `enum XmlNs { Assertion, Protocol, Other }`, track separate `assertion_issuer: Option<String>` and `response_issuer: Option<String>` with an active `<Issuer>` text accumulator (combining `Event::Text` + `Event::CData` chunks and committing/resetting cleanly on `Event::End` so XML comments never split or leak issuer text):
     - If `grand_parent == Some(("Assertion", XmlNs::Assertion))`: record `assertion_issuer`; if already `Some(prev)` with `prev != text`, return `Err(SamlError::SignatureWrappingDetected(...))`.
     - If `grand_parent == Some(("Response", XmlNs::Protocol))`: record first non-empty `response_issuer` as fallback only when `assertion_issuer` is `None`.
     - Ignore any other nested `<Issuer>` (e.g., under `<Advice>`).
   - Replace the ~160 lines of hand-rolled `parse_iso8601_strict`, `parse_tz_offset`, and manual calendar/leap-year math (`saml.rs:133-293`) using `chrono = "=0.4.45"` (already in `Cargo.lock`): wire `chrono` into root `Cargo.toml` `[workspace.dependencies]` and `crates/enterprise/Cargo.toml` (`saml = ["dep:quick-xml", "dep:saml-rs", "dep:chrono"]`), trim input, enforce ISO 8601 `'T'`/`'t'` at byte index 10 (rejecting RFC 3339 space separator), parse with `chrono::DateTime::parse_from_rfc3339(trimmed)`, require non-negative epoch seconds via `u64::try_from(dt.timestamp())` (mapping parse errors and pre-1970 instants to `SamlError::Malformed`), and update `parse_iso8601_strict` doc comments (`:123-130`) to state `Z` or `±HH:MM`.
   - Update module docs (`saml.rs:15-25`) and root `Cargo.toml:64-67` to state accurately that `saml-rs` supplies only `ASSERTION`/`DSIG`/`PROTOCOL` namespace constants while `quick-xml` handles structural parsing and cryptographic signature verification is delegated to `SamlSignatureVerifier`.
3. **Structural Redis session rotation (`crates/security/src/auth/redis.rs`, `session.rs`, `containers/redis.rs`):**
   - Remove `escape_lua_pattern`, `escape_lua_replacement`, and `string.find`/`string.gsub`/`string.match` regex parsing in `REGENERATE_ID_SCRIPT`, updating `regenerate_id` (`redis.rs:221-228`) to pass `ARGV[1] = new_id` and `ARGV[2] = now` (no `old_id_pat`), and updating `redis.rs:256-275` unit tests.
   - Guard JSON decoding in `REGENERATE_ID_SCRIPT`: `local ok, decoded = pcall(cjson.decode, data)`, verify `ok and type(decoded) == 'table' and type(decoded.id) == 'string' and type(decoded.user_id) == 'string'`, parse `local exp = tonumber(decoded.expires_at); if not exp then return redis.error_reply("MALFORMED") end` (propagated as `AuthError::ProviderError`), delete and return `redis.error_reply("EXPIRED")` if `exp <= now`, set `decoded.id = new_id`, clear empty `roles`/`permissions`/`data` tables (`if type(t) == 'table' and next(t) == nil then ... = nil end`) paired with `#[serde(default)]` on `tenant_id`, `roles`, `permissions`, `data` in `Session` (while keeping `id`, `user_id`, `expires_at` required!), and re-encode with `cjson.encode(decoded)`.
   - Extend `test_redis_session_store_save_load_ttl_and_destroy` (`crates/core/tests/containers/redis.rs:646-685`) to exercise `regenerate_id` on both an all-empty `Session::new(...)` (empty `roles`/`permissions`/`data`, `tenant_id: None`) and a fully populated `Session` (`tenant_id`, `roles`, `permissions`, `data`).
4. **Remove `safe_equal_hashed`:** Delete `safe_equal_hashed`, its doctest, its `safe_equal_hashed_tests` unit test, and its `helpers/mod.rs` re-export.

## Constraints honoured

- `rules/10-tech-stack.md`: exact pin `chrono = "=0.4.45"` in root `Cargo.toml` `[workspace.dependencies]`.
- `rules/70-code-rationale.md`:
  - `Why:` comment on `parse_iso8601_strict` explaining why `chrono::DateTime::parse_from_rfc3339` (with explicit `'T'`/`'t'` and non-negative epoch checks) replaces hand-rolled date/offset math (already in `Cargo.lock`, validates leap years/month bounds/`-23:59..=+23:59` offsets safely without UTF-8 slicing, and fails closed on space separators and pre-1970 instants).
  - `Why:` comment on `saml.rs` docs & `Cargo.toml` explaining why `saml_rs::Saml` browser-SSO API is not used and why `exc-c14n`/RSA-SHA256 is intentionally not hand-rolled (`SamlSignatureVerifier` + Experimental status).
  - `Why:` comment on `REGENERATE_ID_SCRIPT` and `Session` `#[serde(default)]` explaining that Redis's embedded Lua `cjson` encodes empty `{}` tables uniformly as `{}` (breaking `Vec<String>`/`BTreeMap` distinction), so omitting empty optional collections in Lua + `#[serde(default)]` preserves strict structural JSON mutation without string hacking.
- `crates/foundation/src/env/loading.rs:412-430` (`file_value_traversal_is_rejected`, `file_value_absolute_path_is_rejected`) and `crates/core/tests/integration/enterprise.rs` (`test_saml_*`).
- `crates/core/tests/containers/redis.rs:646-685` (`test_redis_session_store_save_load_ttl_and_destroy`).

## Named tests (bodies omitted)

- `file_value_nonexistent_and_symlink_escape_fail_closed` (`crates/foundation/src/env/loading.rs` & `crates/cli/src/commands/db/mod.rs` `#[cfg(test)]`) — asserts missing `file:` targets fail closed rather than falling back to uncanonicalized paths, and (Unix) a symlink inside `dir` pointing outside `dir` is rejected.
- `saml_signed_mode_requires_non_empty_assertion_id_and_matching_reference` (`crates/core/tests/integration/enterprise.rs`) — asserts `parse_signed` rejects missing `<ds:Signature>`, empty `<ds:SignedInfo>`, missing/empty `Assertion/@ID`, and `<ds:Reference>` covering only `Response/@ID` with `SignatureWrappingDetected`.
- `saml_issuer_anchoring_and_rfc3339_timestamp_bounds` (`crates/core/tests/integration/enterprise.rs`) — asserts nested `<Advice><Issuer>` cannot spoof Assertion `<Issuer>`, duplicate conflicting Assertion `<Issuer>`s fail closed with `SignatureWrappingDetected`, comment-split `<Issuer>` chunks accumulate cleanly, Response `<Issuer>` acts as fallback when Assertion lacks one, and `parse_iso8601_strict` rejects `+24:00`, `+00:60`, `+1é2`, `+-05:00`, space-separated `"2026-11-02 10:00:00Z"`, and pre-1970 `"1969-12-31T23:59:59Z"` without panicking.
- `regenerate_id_script_uses_guarded_cjson_without_lua_pattern_helpers` (`crates/security/src/auth/redis.rs` `#[cfg(test)]`) — asserts `REGENERATE_ID_SCRIPT` uses `pcall(cjson.decode, data)`, validates `expires_at` before comparing (`MALFORMED`), omits `string.gsub`/`string.find`/`string.match`, and verifies `Session` deserializes when `roles`/`permissions`/`data` are omitted while still rejecting missing `id`/`user_id`/`expires_at`.
- `test_redis_session_store_save_load_ttl_and_destroy` (`crates/core/tests/containers/redis.rs:646-685`) — extended to assert `regenerate_id` succeeds on both an all-empty `Session::new(...)` and a fully populated `Session` (`tenant_id`, `roles`, `permissions`, `data`), and returns `AuthError::ProviderError` if Redis holds corrupt/non-object JSON.

## Verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -j 2 -- -D warnings
cargo test -p rume-foundation --all-features -j 2
cargo test -p rume-enterprise --all-features -j 2
cargo test -p rume-security --all-features -j 2
cargo test -p rume --test integration --all-features enterprise -j 2
cargo test -p rume-cli -j 2
cargo test --workspace --doc --all-features -j 2
```

(And with Docker running: `cargo test -p rume --test containers --all-features test_redis_session_store -j 2`.)

Expected: 0 warnings and all foundation/enterprise/security/CLI/doctests pass.

## Commit

Only when the user asks:

```text
fix(security): harden path canonicalization, SAML validation, and structural Redis session rotation
```

## Done-when

- [ ] Neither `crates/foundation/src/env/loading.rs` nor `crates/cli/src/commands/db/mod.rs` falls back to raw paths on `fs::canonicalize` failure or reads uncanonicalized `&full`.
- [ ] `crates/enterprise/src/enterprise/saml.rs` delegates `parse_iso8601_strict` to `chrono::DateTime::parse_from_rfc3339` (zero `parse_tz_offset`), enforces `signed_mode` reference coverage, `<Issuer>` anchoring, and updated `saml-rs` docs/`Why:` notes alongside `Cargo.toml` (`chrono = "=0.4.45"` and lines 64–67).
- [ ] `! rg -n 'safe_equal_hashed|escape_lua_pattern' crates/` returns 0 matches, `REGENERATE_ID_SCRIPT` uses `pcall(cjson.decode, data)` with `MALFORMED` guard, and Redis container tests cover empty & populated `Session` rotation.
- [ ] All verification commands exit 0 with 0 warnings.
