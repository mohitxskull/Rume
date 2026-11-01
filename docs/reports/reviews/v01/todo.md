# Rume v01 Review — todo.md

Derived from lens reports in `docs/reports/reviews/v01/`:
- `correctness.md` (C1–C4, m1–m4)
- `security.md` (M1–M4, m1–m3)
- `architecture.md` (M1–M2, m1–m2)
- `dx-usability.md` (M1–M3, m1–m5)
- `testing.md` (T1–T9)

Ordered by priority tier (P0 critical → P3 minor), with findings that share a root cause grouped for solving together. IDs are lens-local (e.g. `C1` = correctness finding 1, `S-M2` = security finding M2). A finding listed in two lenses (e.g. Architecture M1 and DX m1 are both "prelude re-exports whole crates") appears once.

---

## P0 — Critical (fix first: live exposure or compile defect everything else builds on)

### C1 — `ValidatedParam<M>` cannot compile with `AppContext` state (`correctness.md` C1)
**Evidence:** `rume/src/routing.rs:186-213` — `impl FromRequestParts<()>` fixes extractor state to `()`, but every Rume app builds `Router<AppContext<E, C>>`. Bundled matchers `matchers::{Number,Uuid,Slug}` do not implement `ParamMatcher`.
**What breaks:** The public `ValidatedParam` extractor is dead on arrival — any handler using it with real `AppContext` state fails to compile.
**Fix:** Make `ValidatedParam` state-generic or provide an `AppContext`-state extraction path; implement `ParamMatcher` for bundled markers.

### S-M1 — `file:` env resolution permits path traversal → arbitrary file read (`security.md` M1)
**Evidence:** `rume/src/env.rs:212–234` — `file:` branch joins `dir` with raw path, no `..` guard, absolute paths accepted wholesale.
**What breaks:** `APP_KEY=file:../../etc/secrets/app.key` reads any file the process can read and injects it as an env var.
**Fix:** Reject `..` segments after canonicalization; reject absolute paths; sandbox resolution under `dir`.

---

## P1 — Major (fix before production launch)

### Group A — Secret handling (shared root cause: secrets treated as plain text)

#### S-M2 — Secrets held as plain `String` in `ResolvedEnv` (`security.md` M2)
**Evidence:** `rume/src/env.rs:29–37` — `ResolvedEnv.values: BTreeMap<String, String>` stores `APP_KEY`, `DB_PASSWORD`, tokens as plain `String`.
**Fix:** Store secret-typed values in `secrecy::SecretBox<String>`; only `expose_secret()` at final consumption point.

#### S-M4 — `AppError` renders `to_string()` into client-facing JSON (`security.md` M4)
**Evidence:** `rume/src/error.rs:56–76` — `Internal`/`BadRequest` bodies use `self.to_string()`, sending secrets/internals to HTTP clients.
**Fix:** Return generic safe messages to clients; log detail server-side with correlation ID.

#### S-M3 — CLI inspection commands print secrets to terminal (`security.md` M3)
**Evidence:** `rume-cli/src/commands/env/check.rs:36–52` (`--all` process-env dump) and `commands/config/check.rs:1–4` (raw config TOML dump).
**Fix:** Default `env:check` and `config check` to redact secret keys and credential URLs; no raw config TOML by default; add explicit `--unredacted` flag.

#### S-m1 — `redact()` heuristic false negatives (`security.md` m1)
**Evidence:** `rume/src/env.rs:237–263` — `is_secret_key()` only matches `SECRET`/`PASSWORD`/`TOKEN`/`_KEY`/`KEY`; benign key names print plaintext.
**Fix:** Broaden detection (`_SECRET`, `_PASS`, `_PWD`, `_CRED`, `_PRIVATE_KEY`, `STRIPE_*`, etc.) or move to an allowlist of known-safe keys.

#### T-T7 — `env.rs` has no `file:` traversal test (`testing.md` T7)
**Evidence:** `rume/src/env.rs` tests cover redaction but not `file:` path traversal (security M1).
**Fix:** Add test that `APP_KEY=file:../../etc/secrets/app.key` is rejected.

---

### Group B — Health check API duplication

#### D-M3 — `health_routes()` vs `ops_router()` are two overlapping APIs (`dx-usability.md` M3)
**Evidence:** `rume/src/routing.rs:553–585` (`health_routes`) and `rume/src/app.rs:536–580` (`ops_router`) — both register `/health/live` and `/health/ready` with different state models.
**Fix:** Merge into one canonical health-route API; document which to use or rename one to signal distinct state model.

#### C-m4 — `/health/ready` returns 503 for `Warning`, not just failures (`correctness.md` m4)
**Evidence:** `rume/src/routing.rs:486-496` — `is_healthy` is false for `Warning` status, so a warning check triggers 503.
**Fix:** Decide intentionally: readiness = fully healthy (503 on warning) or readiness = degraded-but-serving (200 on warning); align code with doc wording.

#### S-m2 — `/health/ready` exposes pid and platform in `debug_info` (`security.md` m2)
**Evidence:** `rume/src/routing.rs:458, 531–542` — `debug_info()` emits PID and OS into every `/health/ready` response.
**Fix:** Remove `pid`/`platform` from `debug_info()`, or gate behind development-only flag.

#### T-T8 — `http_smoke.rs` tests a bare router, not real health routes (`testing.md` T8)
**Evidence:** `rume/tests/http_smoke.rs` builds `Router::new().route("/health", get(health))` — never exercises `health_routes` or `ops_router`.
**Fix:** Add integration test against real `health_routes` with `AppContext` state, availability transitions, and `HealthChecks::run`.

---

### Standalone P1 findings

#### C-C2 — `ApplicationMode::Inspect` still serves HTTP traffic (`correctness.md` C2)
**Evidence:** `rume/src/app.rs:47-53` documents Inspect as "without serving"; `run()` (`app.rs:404-451`) calls `axum::serve` unconditionally; only `on_ready` hooks are gated on `mode == Run`.
**Fix:** Guard `axum::serve`/listening behind `mode == Run`, or otherwise honor the Inspect contract.

#### D-M1 — `AppContext::new()` takes 6 positional args (`dx-usability.md` M1)
**Evidence:** `rume/src/app.rs:242-249` — `AppContext::new(env, config, paths, availability, health_checks)`; call sites must make manual clone decisions.
**Fix:** Replace with builder-style constructor (`AppContext::builder().env().config().paths().availability().health_checks().build()`) or provide `new_with_defaults` for common case.

#### T-T1 — `routing.rs` has ZERO unit tests (`testing.md` T1)
**Evidence:** `rume/src/routing.rs` (all ~600 lines) has no `#[cfg(test)]` module; Matcher edge cases, `ValidatedParam`, `RouteGroup`, `HealthChecks::run`, `health_routes`, `Rejection`, `ROUTE_REGISTRY` all untested.
**Fix:** Add `#[cfg(test)]` coverage for all untested behaviors in `routing.rs`.

---

## P2 — Major robustness (fix before later rollout)

### C-C3 — `Lifecycle::set`/`Availability::set` accept any `u8` (`correctness.md` C3)
**Evidence:** `rume/src/app.rs:86-98`, `140-152` — `state` stored as `u8` with no range validation; `state()` default arm silently maps invalid values to `Created`/`Unknown`.
**Fix:** Add `#[repr(u8)]` with explicit discriminants to `LifecycleState`/`AvailabilityState`, or store state as enum directly.

### A-M1 / D-m1 — Prelude re-exports whole third-party crates (`architecture.md` M1 / `dx-usability.md` m1)
**Evidence:** `rume/src/lib.rs:30–33` re-exports `axum`, `serde`, `tokio`, `tracing`; `prelude.rs:18-24` adds `axum::routing`, `State`, `Path`, `Json`.
**Fix:** Narrow prelude to Rume's own types plus small extractor set (`State`, `Path`, `Json`); remove whole-crate re-exports.

### A-M2 / D-M2 — `RumeBuilder::new` is public but `builder()` documented as sole entry point (`architecture.md` M2 / `dx-usability.md` M2)
**Evidence:** `rume/src/app.rs:274-282` documents `Rume::builder` as "the only way"; `RumeBuilder::new` at line 307 is `pub` and equivalent.
**Fix:** Make `RumeBuilder::new` `pub(crate)` so `Rume::builder` is genuinely the only public entry point, or update doc comment to acknowledge both.

### T-T2 — No fails-before proof anywhere in the suite (`testing.md` T2)
**Evidence:** Every test file passes, but no test was demonstrated to fail before its fix; no committed failing test or `todo_*` stub.
**Fix:** Add fails-before artifacts for stage-1 gaps being fixed (C1, C2, C3, m1–m4) — committed failing tests that pass after the fix.

---

## P3 — Minor hardening

### C-m1 — `Matcher::Number` doc ("digits only") does not match `u64`-bounded parsing (`correctness.md` m1)
**Evidence:** `rume/src/routing.rs:47-65` — `parse::<u64>()` rejects overflowing digit strings; returns original string, not parsed number.
**Fix:** Update doc to match behavior, or cast to parsed number and document overflow behavior.

### C-m2 — `RouteGroup::add` does not normalize path separators (`correctness.md` m2)
**Evidence:** `rume/src/routing.rs:400-408` — `format!("{}{}", prefix, path)` with no slash handling; `prefix("/api/").add("/users")` → `/api//users`, `prefix("api").add("users")` → `apiusers`.
**Fix:** Normalize separators when joining prefix + path.

### C-m3 — `RouteSpec::build()` registers in `ROUTE_REGISTRY` even when never mounted (`correctness.md` m3)
**Evidence:** `rume/src/routing.rs:353-363` — `register_route(info)` called unconditionally inside `build()`.
**Fix:** Register at mount time, not build time; or document that build-time registration is intentional.

### S-m3 — `Rejection` echoes parameter values/names in JSON error responses (`security.md` m3)
**Evidence:** `rume/src/routing.rs:57–81` — `Matcher::validate` builds `"{value} is not a valid ..."`; `Rejection::IntoResponse` returns `{"error": message}`.
**Fix:** Return generic messages naming the parameter and expected format without echoing raw value; or echo only in development mode.

### D-m2 — `rume generate handler` uses fragile string-marker codegen (`dx-usability.md` m2)
**Evidence:** `rume-cli/src/commands/generate/handler.rs:62-90` — reads `src/routes.rs`, searches for literal `// rume:routes`, inserts route by string concatenation; missing marker → warning but continues.
**Fix:** Treat missing marker as error (exit non-zero), or generate routes into a separate included file for compile-time checking.

### D-m3 — `rume new` scaffolds local-path `rume` dependency with no warning (`dx-usability.md` m3)
**Evidence:** `rume-cli/src/commands/new.rs:6` — `FRAMEWORK_PATH` points at local checkout; `Cargo.toml.tpl:7` uses `path = "{{framework_path}}"`.
**Fix:** Default template to crates.io version pin or clearly-marked placeholder; add post-scaffold note to verify/update `rume` dependency before `cargo run`.

### D-m4 — Generated code imports via `rume::axum`/`rume::serde` (`dx-usability.md` m4)
**Evidence:** `rume-cli/src/commands/generate/handler.rs:23-24` generates `use rume::axum::Json; use rume::serde::Serialize;`.
**Fix:** Generate imports from crates directly (`use axum::Json; use serde::Serialize;`); add `axum` and `serde` as direct dependencies in template `Cargo.toml`.

### D-m5 — No machine-readable output for `doctor`, `env check`, `config check` (`dx-usability.md` m5)
**Evidence:** `rume-cli/src/commands/doctor.rs:94-98`, `env/check.rs:49`, `config/check.rs` — all human-readable text only.
**Fix:** Add `--format json`/`--json` flag to `doctor`, `env check`, `config check` emitting structured output; pair with secret redaction by default in `config check`.

### T-T3 — `rume/src/config.rs` entirely untested (`testing.md` T3)
**Evidence:** `rume/src/config.rs` has no `#[cfg(test)]` module; `build_figment` candidate precedence, `NotFound` error path, env-layering, `schemars` feature path all untested.
**Fix:** Add `#[cfg(test)]` coverage for `load`/`load_in`/`load_raw`, error paths, env-layering precedence.

### T-T4 — `TestApp`/`TestAppBuilder` test harness is itself untested (`testing.md` T4)
**Evidence:** `rume/src/testing.rs` has no `#[cfg(test)]` module; `validation.rs` tests depend on `TestApp::new`, `post_json`, `body_text`.
**Fix:** Add tests for `TestApp`/`TestAppBuilder` methods so the test oracle is itself verified.

### T-T5 — `app.rs` tests miss invalid-`u8` boundary, Inspect mode, lifecycle ordering (`testing.md` T5)
**Evidence:** `rume/src/app.rs` tests cover happy-path transitions only; no test for invalid `u8` (C3), Inspect mode non-serving (C2), or `run()` lifecycle ordering.
**Fix:** Add tests for `set()` with invalid `u8`, Inspect mode not serving, and `run()` state transitions.

### T-T6 — `error.rs` tests cover 3 of 6 `AppError` variants and assert no response bodies (`testing.md` T6)
**Evidence:** `rume/src/error.rs` tests cover `NotFound`, `Validation`, `Internal` only; `BadRequest`, `Unauthorized`, `Forbidden` untested; no `IntoResponse` body assertions.
**Fix:** Test all 6 variants; assert JSON body shape (`{"error": …}` vs `{"error": …, "details": …}`).

### T-T9 — No test isolates or cleans up `ROUTE_REGISTRY` global state (`testing.md` T9)
**Evidence:** `rume/src/routing.rs` `ROUTE_REGISTRY: OnceLock<Mutex<Vec<RouteInfo>>>` is never reset between tests; `RouteSpec::build()` calls `register_route()` unconditionally.
**Fix:** Add test that registry reflects only mounted routes; add reset/cleanup between tests; test build-without-mount pollution.

---

## ## Counts

| Tier | Count | Findings |
|------|-------|----------|
| **P0** | 2 | C1, S-M1 |
| **P1** | 12 | C-C2, D-M1, T-T1 (standalone); S-M2, S-M4, S-M3, S-m1, T-T7 (Group A); D-M3, C-m4, S-m2, T-T8 (Group B) |
| **P2** | 4 | C-C3, A-M1/D-m1, A-M2/D-M2, T-T2 |
| **P3** | 13 | C-m1, C-m2, C-m3, S-m3, D-m2, D-m3, D-m4, D-m5, T-T3, T-T4, T-T5, T-T6, T-T9 |

Total findings: 31 unique (35 lens findings, 4 appearing in two lenses: A-M1/D-m1, A-M2/D-M2, plus 2 cross-lens duplicates accounted above)

**Solving groups:**
- **Group A** (Secret handling): S-M2, S-M4, S-M3, S-m1, T-T7 — one fix (secrecy::SecretBox + redaction + traversal test)
- **Group B** (Health check API): D-M3, C-m4, S-m2, T-T8 — one fix (unify health routes, fix severity mapping, test real routes)
- **Group C** (Missing test coverage): T-T1, T-T3, T-T4 — one fix (add tests for routing.rs, config.rs, testing.rs)

**Ruling:** Fix P0 first (compile blocker + arbitrary file read). Then P1 Group A + Group B + standalone in any order (all block production use). Then P2, then P3.