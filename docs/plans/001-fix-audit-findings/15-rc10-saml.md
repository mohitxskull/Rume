# 15-rc10-saml.md

## Goal

Fix RC-10 SAML 2.0 signature-verification bypasses in `crates/core/src/enterprise/saml.rs`:

1. Empty `signature_references` bypass: `parse_internal` collects `Reference` URIs into `signature_references` (~line 365, populated ~lines 413-421), but when the vector is empty the validation loop (~lines 539-557) simply never runs, so a signed assertion with no references passes the reference check without the signature covering the assertion or response. Fix: in `parse_signed`/`with_verifier` mode (signature mandatory), reject empty `signature_references` as `SignatureWrappingDetected` — a signature must reference the assertion ID, response ID, or root document.
2. `parse_iso8601_strict` is permissive: it accepts raw integers (`s.parse::<u64>()` at line 117) and accepts timestamps with no timezone (the `else` branch at lines 175-177 defaults `tz_offset = 0`). Strict ISO 8601 must require an explicit timezone designator (`Z` or `±HH:MM`/`±HHMM`). Fix: remove the raw-integer shortcut (or keep it only if it is itself a valid strict timestamp) and require a timezone, erroring on missing/ambiguous offsets.

## Files touched

- `crates/core/src/enterprise/saml.rs` — signature reference validation (~lines 538-557); `parse_iso8601_strict` (~lines 115-237).

## Constraints honoured

- Root cause, not symptom: reject empty references at the source rather than tolerating them; require timezone at parse time rather than tolerating naive timestamps.
- No new dependencies, no `unsafe`.
- `parse_iso8601_strict` remains strict: `Z`, `+HH:MM`, `-HH:MM`, `+HHMM`, `-HHMM` accepted; naive and raw-integer forms rejected.
- Record `Why:` comments for the empty-reference rejection and the timezone requirement.

## Done when (checkable by a command)

- `cargo test -p rume-core enterprise` passes (SAML tests).
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `parse_iso8601_strict("2024-01-01T10:00:00")` (no TZ) returns an error; `parse_iso8601_strict("2024-01-01T10:00:00Z")` succeeds.
- Empty `signature_references` in signed mode returns `SignatureWrappingDetected`.

## Verification (fails-before test idea)

Tests:
1. **Empty references:** build a SAML assertion with a `Signature` element but zero `Reference` children, parse in signed mode. **Before fix** passes (bypass) → test fails; **after fix** returns `SignatureWrappingDetected` → passes.
2. **Timezone required:** `parse_iso8601_strict("1704067200")` (raw integer) and `parse_iso8601_strict("2024-01-01T10:00:00")` (no TZ) both return `Malformed`; `parse_iso8601_strict("2024-01-01T10:00:00Z")` and `"+05:30"` offset forms succeed. Before the fix these are accepted → test fails.
