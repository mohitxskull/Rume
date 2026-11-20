# Task 11 — Rename `crates/foundation/src/helpers/` → `crates/foundation/src/util/`, update references

## Goal

`crates/foundation/src/helpers/` renamed to `crates/foundation/src/util/`; every reference updated
(`rume_foundation::helpers` → `rume_foundation::util`, `rume::helpers` → `rume::util`);
`cargo build` + per-crate tests green after the task.

## Files touched

**Rename (git mv):**
- `crates/foundation/src/helpers/` → `crates/foundation/src/util/` — `mod.rs`, `bytes.rs`, `case.rs`,
  `duration.rs`, `html.rs` (split in Task 12), `mask.rs`, `ordinal.rs`, `safe_equal.rs`,
  `sentence.rs`, `slug.rs`, `truncate.rs`, `whitespace.rs`.

**`crates/foundation/src/lib.rs`:**
- Line 9: `pub mod helpers;` → `pub mod util;`.

**Internal `super::` refs (unchanged — resolve under the renamed module):**
- `crates/foundation/src/util/html.rs` (pre-split): `use super::truncate::truncate;` /
  `use super::whitespace::condense_whitespace;` — from `util::html`, `super::truncate` = `util::truncate` ✓.
- (Task 12 moves these to `util/html/` with `use crate::util::truncate::…` paths.)

**Doc-comment path updates (`rume_foundation::helpers::` → `rume_foundation::util::`):**
- `crates/foundation/src/util/safe_equal.rs:20`, `util/mask.rs:11,46`, `util/duration.rs:32,98`,
  `util/sentence.rs:10,25`, `util/bytes.rs:36,73`, `util/truncate.rs:32`, and `util/html.rs` doc
  examples (updated in Task 12).

**`crates/core/src/lib.rs`:**
- Line 30: `pub use rume_foundation::helpers;` → `pub use rume_foundation::util;`.
- Line 12 (Tier-1 doc list): "`…, module, routing, helpers)" → "…, module, routing, util)".

**`crates/security/src/security/csrf.rs:14`:** `use rume_foundation::helpers::safe_equal;` →
`use rume_foundation::util::safe_equal;`.

**`crates/cli/src/commands/generate/{resource,service,model,handler}.rs`:** `rume::helpers::` →
`rume::util::` (`pascal_case` calls).

**`crates/cli/src/commands/env/explain.rs:77`:** `rume::helpers::truncate(...)` → `rume::util::truncate(...)`.

**`crates/core/tests/integration/property_tests.rs:11-17`:** `rume::helpers::…` → `rume::util::…`
(`bytes::{format_bytes, parse_bytes}`, `case::{…}`, `duration::{…}`, `mask::mask_secret`,
`slug::slug`, `truncate::truncate`, `whitespace::condense_whitespace`).

## Deliverable

- `rume_foundation::util::{bytes, case, duration, html, mask, ordinal, safe_equal, sentence,
  slug, truncate, whitespace}` exist; `rume_foundation::helpers` is gone.
- `rume::util::…` (via `core/src/lib.rs` re-export); `rume::helpers` is gone.
- No `rume_foundation::helpers::` / `rume::helpers::` references remain anywhere in `crates/`
  (grep: `grep -rn "rume_foundation::helpers\|rume::helpers" crates --include=*.rs` → empty).

## Constraints honoured

- Global constraints: pure refactor; `cargo build` + `cargo test -p rume-foundation -p
  rume-security -p rume-cli -p rume` green (see Verification).
- Phase 3 decision: rename `helpers/` → `util/`.
- Out of scope: `docs/` mentions of `helpers/` not updated (Flagged gaps in README).

## Named tests

All tests must stay green (moved with the files):
- `crates/foundation/src/util/*` unit tests (bytes, case, duration, mask, ordinal, sentence, slug,
  truncate, whitespace, html [Task 12]) — e.g. `safe_equal` constant-time behaviors, `truncate`
  truncation semantics, `case` roundtrips, `duration` parsing, `mask` redaction — via
  `cargo test -p rume-foundation`.
- `crates/security/src/security/csrf.rs` `mod tests` — via `cargo test -p rume-security`.
- `crates/cli/src/commands/generate/*` + `crates/cli/src/commands/env/explain.rs` — via
  `cargo test -p rume-cli`.
- `crates/core/tests/integration/property_tests.rs` — via `cargo test -p rume`.

## Verification

```
cargo build
cargo test -p rume-foundation
cargo test -p rume-security
cargo test -p rume-cli
cargo test -p rume
grep -rn "rume_foundation::helpers\|rume::helpers" crates --include=*.rs   # expect: empty
ls crates/foundation/src/helpers                                          # expect: No such file or directory
```

## Commit

Only when the user asks:

```
refactor(foundation): rename helpers/ to util/ and update rume_foundation::helpers callers
```

## Done-when

- [ ] `crates/foundation/src/helpers/` no longer exists; `crates/foundation/src/util/` exists with
  `mod.rs` + `bytes/case/duration/html/mask/ordinal/safe_equal/sentence/slug/truncate/whitespace`
  (html split per Task 12).
- [ ] `rume_foundation::util` resolves; `rume::util` resolves (core re-export updated).
- [ ] `grep -rn "rume_foundation::helpers\|rume::helpers" crates --include=*.rs` → empty.
- [ ] `cargo build` + `cargo test -p rume-foundation -p rume-security -p rume-cli -p rume` green.
