# Testing lens report — Rume v01 whole-document review

**Report file:** `docs/reports/reviews/v01/testing.md`
**Lens:** Testing — sufficient coverage? fails-before proof? Are tests hermetic?
**Code reviewed:** `crates/core/src/{app,error,env,validation,routing,config,testing}.rs`; `crates/core/tests/{env_and_config_loading,http_smoke}.rs`
**Method:** whole-document, stage 2 (depends on stage 1: correctness.md, security.md, architecture.md). No behavior was executed; this lens judges test coverage, fails-before proof, and hermeticity against the code and the stage-1 gaps.

---

## Findings

### T1 (Major) — `crates/core/src/routing.rs` has ZERO unit tests despite being the largest new module

**Where:** `crates/core/src/routing.rs` (entire file, lines 1–611) — no `#[cfg(test)] mod tests` anywhere.

**What is missing:** Every behavior in the module is untested:
- `Matcher::{Number,Uuid,Slug}::validate` (lines 57–81) — no edge-case coverage for UUID segment lengths/hex digits, slug leading/trailing `-`/`_`, `--`/`__`, empty values, or `Number`'s `u64` overflow (correctness m1).
- `ValidatedParam<M>` extractor (lines 133–213) — no test that it compiles/works against `AppContext` state, which correctness C1 shows it does not.
- `RouteSpec::{get,post,put,patch,delete,where_param,r#where,r#as,build}` (lines 282–364) — `build()` has no test at all.
- `RouteGroup::{new,prefix,r#as,add,build}` (lines 371–420) — no test that `add` normalizes path separators (correctness m2: `/api/` + `/users` → `/api//users`, `api` + `users` → `apiusers`).
- `HealthChecks::run` (lines 481–508) — no test that `Warning` → `is_healthy == false` → 503 (correctness m4).
- `health_routes` (lines 553–585) — no test of `/health/live` (503 only on `Broken`) or `/health/ready`.
- `Rejection::IntoResponse` (lines 599–610) — no test of the 400/400/422 status mapping (correctness check #62).
- `registered_routes()` / `ROUTE_REGISTRY` (lines 229–248) — no test of registry contents or cross-test pollution (correctness m3).

**What breaks:** The module with the most surface area and the most stage-1 findings (C1, m1–m4) has no test that would catch a regression. Any fix to C1 (state-generic `ValidatedParam`), m1 (doc/impl mismatch), m2 (path normalization), m4 (Warning semantics), or m3 (registry pollution) ships without a regression guard. The break: a production change to any of these can pass `cargo test` while being wrong.

### T2 (Major) — No fails-before proof anywhere in the suite

**Where:** Every test file: `crates/core/src/{app,error,env,validation}.rs` `#[cfg(test)]` modules and `crates/core/tests/{env_and_config_loading,http_smoke}.rs`.

**What it says:** `cargo test` passes (19 tests per plan.md:21), but no test in the repository was demonstrated to fail before its fix. There is no committed failing test, no `todo_*` stub, no "this test fails on the unfixed code" artifact for any finding. The testing convention (`.opencode/rules/testing.md`) makes TDD the default and requires fails-before proof: "Regression tests must demonstrably fail before the fix (fails-before), pass after."

**What breaks:** Without fails-before evidence, a test that passes on current code is indistinguishable from a tautological test that sleeps through bugs. A reviewer cannot tell whether `maps_status_codes` (error.rs:83) guards the status mapping or merely restates it; whether `invalid_payload_returns_422_with_details` (validation.rs:107) catches a real garde-mapping regression or a copy-paste of the implementation. The break: regressions introduced after this review can pass the suite undetected because no test has proven it catches anything.

### T3 (minor) — `crates/core/src/config.rs` is entirely untested

**Where:** `crates/core/src/config.rs` (lines 1–100) — no `#[cfg(test)]` module.

**What is missing:** `load`/`load_in`/`load_raw`/`load_raw_pretty` (lines 32–58), `build_figment` candidate precedence (lines 60–88), the `NotFound` error path when no TOML file exists (lines 81–85), env-layering (`app.toml` → `app.{env}.toml` → `app.local.toml` → `app.{env}.local.toml`), and the `schemars` feature path (lines 93–100).

**What breaks:** config loading is a boot-critical path (Env/Config split per architecture). A precedence bug, a missing-file error that surfaces as the wrong variant, or a feature-gated `schema()` failure ships without coverage. The integration test `loads_merged_config` (env_and_config_loading.rs:100) exercises `config::load_raw`/`load_in` indirectly, but only one happy-path case with two files — no error path, no precedence beyond two layers, no `NotFound`.

### T4 (minor) — the `TestApp`/`TestAppBuilder` test harness is itself untested

**Where:** `crates/core/src/testing.rs` (lines 1–145) — no `#[cfg(test)]` module.

**What is missing:** `TestApp::{new,with_cleanup,get,post_json,request,body_bytes,body_text,teardown,Drop}`, `TestAppBuilder::{new,router,route,with_cleanup,build,http}`, and the `status` helper (lines 143–145). `validation.rs` tests (lines 97–142) depend on `TestApp::new`, `post_json`, and `body_text`; if the harness is broken, those tests' results are untrusted.

**What breaks:** Untested test infrastructure means the oracle for every `validation.rs` HTTP test is itself unverified. A `body_text` that silently drops bytes, a `post_json` that sets the wrong content type, or a `request` that swallows errors would make validation tests pass for the wrong reason.

### T5 (minor) — `app.rs` tests miss the boundary, Inspect-mode, and lifecycle-ordering surface

**Where:** `crates/core/src/app.rs` tests (lines 481–506): `environment_normalizes`, `availability_transitions`, `lifecycle_transitions`.

**What is missing:**
- `Lifecycle::set`/`Availability::set` with an invalid `u8` (5, 255) — correctness C3: any `u8` is stored; `state()` default arm silently maps to `Created`/`Unknown`. No test asserts behavior for out-of-range input.
- `ApplicationMode::Inspect` — correctness C2: `run()` serves HTTP unconditionally. No test asserts Inspect does NOT bind/serve.
- `run()` lifecycle ordering (Booted→Live→Ready→serve→Terminating→RefusingTraffic→Terminated→Unknown) — no test drives `run()` to completion or asserts state transitions across it.
- `on_ready`/`on_shutdown` hook execution gated by `mode == Run`.

**What breaks:** The only state-machine tests are two happy-path transitions. The boundary (C3) and the mode contract (C2) that stage 1 flagged are untested, so a fix to either cannot be regression-guarded.

### T6 (minor) — `error.rs` tests cover 3 of 6 `AppError` variants and assert no response bodies

**Where:** `crates/core/src/error.rs` tests (lines 82–93): `maps_status_codes`.

**What is missing:** `BadRequest`, `Unauthorized`, `Forbidden` variants are never passed to `status_code()`. No `IntoResponse` test asserts the JSON body shape (`{"error": …}` vs `{"error": …, "details": …}`) or the status/body pairing for any variant. Security M4 (error bodies echo internals to clients) has no test.

**What breaks:** Half the enum is untested, and the client-facing body contract — the exact leakage surface flagged in security — is not asserted. A change that drops `details` from `Validation` or changes a status code for `Unauthorized` would not be caught.

### T7 (minor) — `env.rs` has no `file:` traversal test, and the integration env tests mutate global process env (not hermetic)

**Where:** `crates/core/src/env.rs` tests (lines 269–296): `redacts_secret_keys`, `redacts_credential_urls`, `detects_secret_keys`; `crates/core/tests/env_and_config_loading.rs` (lines 10–20): `set_env`/`remove_env` use `unsafe { std::env::set_var/remove_var }` guarded by `ENV_LOCK` Mutex.

**What is missing (src):** No test for `file:` path traversal — security M1 (`APP_KEY=file:../../etc/secrets/app.key` or `file:/etc/passwd` read arbitrary files, no `..` guard). No test for `resolve_env_name` precedence (`RUME_ENV` > `RAILS_ENV` > `NODE_ENV` > `development`). No test that `file\:` escape works.

**Hermeticity:** `env_and_config_loading.rs` mutates the process environment for `RUME_ENV`, `HOST`, `PORT`, `SECRET`, `LOG_LEVEL` via `unsafe` and a process-wide `ENV_LOCK`. Tests in the same binary share that mutable global; a test that forgets `remove_env` leaks into the next test, and parallel test execution (default in `cargo test`) races on the lock-protected globals. This is not hermetic per `.opencode/rules/testing.md` ("Pure domain/policy logic → hermetic `#[cfg(test)]` unit tests"; DB/HTTP surface → `tests/` via harness). The env tests straddle both worlds.

**What breaks:** The exact `file:` traversal primitive flagged in security M1 has no test, so a "fix" that only partially sanitizes cannot be verified. Global env mutation means a flaky or order-dependent failure can appear only under parallel execution — non-deterministic and hard to debug.

### T8 (minor) — `http_smoke.rs` tests a bare router, not the real health surface

**Where:** `crates/core/tests/http_smoke.rs` (lines 1–24): `health_endpoint_returns_ok` builds `Router::new().route("/health", get(health))` with a hand-rolled handler.

**What is missing:** No test exercises the real `health_routes` (`routing.rs:553`) or `ops_router` (`app.rs:536`) — the production health endpoints `/health/live` and `/health/ready` with `AppContext` state, availability transitions, and `HealthChecks::run`. The smoke test's `/health` path does not even match the real `/health/live`/`/health/ready` routes.

**What breaks:** The only HTTP integration test does not touch the actual health surface that security m2 (pid/platform in `debug_info`) and correctness m4 (Warning → 503) concern. Regressions in `health_routes` pass `cargo test` because the smoke test bypasses it entirely.

### T9 (minor) — No test isolates or cleans up `ROUTE_REGISTRY` global state across tests

**Where:** `crates/core/src/routing.rs` lines 229–248 (`ROUTE_REGISTRY: OnceLock<Mutex<Vec<RouteInfo>>>`, `register_route`, `registered_routes`); `RouteSpec::build` calls `register_route` unconditionally (line 360).

**What is missing:** No test asserts that `registered_routes()` reflects only mounted routes, no test clears/resets the registry between tests, and no test demonstrates that building a route without mounting it pollutes the registry (correctness m3).

**What breaks:** `OnceLock` is never reset for the test binary's lifetime, so registry entries accumulate across tests in the same process. A test that builds routes and then asserts `registered_routes()` is empty would be order-dependent. The m3 pollution path (build-without-mount) is untested, so a fix to make registration mount-time would have no regression guard.

---

## Positive notes

- `validation.rs` tests (lines 97–142) are the strongest in the suite: three `#[tokio::test]` cases with hand-derived expectations (200 OK, 422 with `details` array containing `username`/`email`/`age`, 400 on missing required field), asserting body structure rather than computing expected values from the code under test.
- `app.rs` tests (lines 481–506) are hermetic: they construct `Environment`/`Availability`/`Lifecycle` directly instead of mutating process env, and assert real state transitions.
- `env.rs` src tests (lines 269–296) are clear and focused: `redacts_secret_keys` covers a redaction hit and a miss; `redacts_credential_urls` covers the `user:pass@host` URL heuristic; `detects_secret_keys` covers positive and negative key names.
- `env_and_config_loading.rs` integration tests (lines 26–152) use `TempDir` for isolation, test layered precedence (`.env` → `.env.{env}` → `.env.local`), test-env skipping of `.env.local`, `file:` resolution, merged config precedence, and typed env deserialization with `IpAddr`/`u16` parsing.
- `http_smoke.rs` exists, passes, and uses the `TestApp` harness rather than a raw pool or hand-built server — it follows the integration-test placement rule even though it targets the wrong router.

---

## Verdict

**Fail — conditional.**

Testing coverage is insufficient and fails-before proof is absent. Two Major findings:

1. **T1** — `routing.rs`, the largest module and the one carrying the most stage-1 findings (C1, m1–m4), has zero unit tests. Matcher edge cases, `ValidatedParam` against real state, `RouteGroup` normalization, `HealthChecks::run`/Warning→503, `health_routes`, `Rejection` status mapping, and `ROUTE_REGISTRY` pollution are all untested.
2. **T2** — No test in the repository demonstrates fails-before proof, violating the TDD default required by `.opencode/rules/testing.md`. Passing tests cannot be distinguished from tautological ones.

Conditions to pass: add `#[cfg(test)]` coverage for `routing.rs` (all untested behaviors above), `config.rs` (including the `NotFound` error path), and `testing.rs` (the harness itself); add fails-before artifacts (committed failing tests or equivalent proof) for at least the stage-1 gaps being fixed (C1, C2, C3, m1–m4); make `env.rs`/`env_and_config_loading.rs` hermetic or explicitly isolate process-env mutation; add a `file:` traversal test (security M1); and extend `error.rs` tests to all six variants plus body assertions. The seven minor findings (T3–T9) should be closed in the same pass but do not block the verdict on their own.

The foundation is sound where it exists (`validation.rs`, `app.rs`, `env.rs` unit tests are well-written); the problem is scope and proof, not style.
