# 23-rc19-rc21-code-hygiene.md

## Goal

Fix RC-19 dead/deprecated code and RC-21 comment/code divergences (LOW, hygiene).

RC-19 dead/deprecated code: remove or clearly deprecate `ops_router`, `SamlAssertion::parse` (already `#[deprecated]` — confirm callers), `SCHEMA` alias, `update_patch` alias, `into_routes`, and the `CsrfToken` struct if unused. NOTE: `crates/xtask/` does not exist and `EnvError::MissingRequired` does not exist — do not write tasks for those; confirm absence before removal.

RC-21 comment/code divergences:
- `crates/core/src/persistence/mysql.rs` inline SQL — comment describes different SQL than what executes.
- `crates/core/src/routing/health.rs` TTL comment — comment describes caching behavior that doesn't match code.
- `LogEnv` dead code — comment references behavior not present.

Fix root cause: align code with comments or comments with code; remove genuinely dead code (after confirming no callers).

## Files touched

- `crates/core/src/persistence/mysql.rs` — align inline SQL with its comment (or vice versa).
- `crates/core/src/routing/health.rs` — align TTL comment with caching behavior.
- `LogEnv` — locate and remove dead code or align comment.
- RC-19 items — locate each and remove/deprecate after confirming no callers (grep first).

## Constraints honoured

- Root cause, not symptom: dead code removed (not commented out); comments describe actual behavior.
- No new dependencies, no `unsafe`.
- Confirm no callers via grep before removing anything; if a caller exists, keep and align instead.
- Record `Why:` comments for each removal/alignment decision.

## Done when (checkable by a command)

- `cargo build` and `cargo test` pass (whole workspace) — no dead-code warnings introduced or uncovered.
- `cargo clippy --workspace --all-targets` and `cargo fmt --all --check` clean.
- Grep confirms each removed item has zero remaining references.
- Comments in mysql.rs, health.rs, and LogEnv match actual behavior.

## Verification (fails-before test idea)

Tests:
1. **Dead code:** grep for each RC-19 item (`ops_router`, `SCHEMA`, `update_patch`, `into_routes`, `CsrfToken`, `LogEnv`) → confirm zero callers → removal compiles and `cargo build` is clean. **Before fix** the item exists but is unused (dead-code warning or lint) → test (build with `-D warnings`) fails.
2. **Comment divergence:** extract the comment's claimed behavior (e.g. health.rs TTL comment claims X) and assert the code actually does X; where they diverge, align them. **Before fix** assertion on comment-claimed behavior fails → test fails; after alignment passes.
