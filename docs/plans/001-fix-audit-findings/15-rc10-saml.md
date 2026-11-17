# 15-rc10-saml.md

## OVERRIDE — Dependency-based approach
Do NOT hand-roll SAML XML parsing. Instead:
- Add the `saml-rs` crate to workspace dependencies and core Cargo.toml
- Rewrite `crates/core/src/enterprise/saml.rs` to use `saml-rs` for assertion parsing and signature verification
- `saml-rs` handles `<ds:Reference>` enforcement, element depth tracking, and strict ISO 8601 timestamp parsing correctly

## Task file
Read the full task file: /home/skull/Projects/rume/docs/plans/001-fix-audit-findings/15-rc10-saml.md

## Your job
Fix RC-10 SAML parser defects using saml-rs:

1. Add `saml-rs` crate to workspace `Cargo.toml` (`[workspace.dependencies]`) and `crates/core/Cargo.toml`
2. Replace hand-rolled `SamlParser::parse_internal` with `saml-rs` for SAML assertion parsing
3. `saml-rs` handles empty `<ds:Reference>` rejection, element depth/parent tracking, and strict ISO 8601 timezone requirement
4. Preserve public API: `SamlParser::parse`, `parse_verified`, `parse_unverified`, `SamlAssertion`

## Files touched
- `Cargo.toml` (workspace deps)
- `crates/core/Cargo.toml` (add saml-rs)
- `crates/core/src/enterprise/saml.rs` — rewrite to use saml-rs

## Constraints
- Use saml-rs for SAML parsing and signature verification
- No `unsafe`
- Record `Why:` comments at decision sites
- Empty `signature_references` in signed mode must return `SignatureWrappingDetected`
- `parse_iso8601_strict` must require explicit timezone (`Z` or `±HH:MM`/`±HHMM`), reject raw integers and naive timestamps

## Verification
1. `cargo test -p rume-core enterprise`
2. `cargo clippy -p rume --all-targets`
3. `cargo fmt --all --check`
4. `parse_iso8601_strict("2024-01-01T10:00:00")` (no TZ) returns error; `parse_iso8601_strict("2024-01-01T10:00:00Z")` succeeds

## Handoff
Write to `/home/skull/Projects/rume/docs/plans/001-fix-audit-findings/scratch/task-15-done.md`. Do not commit.
