# Correctness lens report — Rume v01 whole-document review

**Report file:** `docs/reports/reviews/v01/correctness.md`
**Lens:** Correctness — does the code do what it claims?
**Code reviewed:**
- `crates/core/src/routing.rs` — `Matcher`, `ValidatedParam<M>`, `RouteSpec`, `RouteGroup`, `health_routes`, `ROUTE_REGISTRY`, `Rejection`
- `crates/core/src/app.rs` — `Lifecycle`/`Availability` state machines, `RumeBuilder::run()`, `ops_router`
- `crates/core/src/error.rs` — `AppError` status mapping + `IntoResponse`
- `crates/core/src/validation.rs` — `Validated<T>` extractor (garde → 400/422)

**Method:** whole-document, stage 1 (independent). One behavior was verified empirically: a temporary compile check combining `ValidatedParam` with `State<AppContext<(),()>>` (the file was deleted after the check; the error is reproduced verbatim below).

---

## Findings

### C1 (Major) — `ValidatedParam<M>` cannot be used with the framework's real state (`AppContext`)

**Evidence:** `crates/core/src/routing.rs:186-213` — `impl<M> FromRequestParts<()> for ValidatedParam<M>` fixes the extractor state to `()`. Every Rume application builds `Router<AppContext<E, C>>` (`crates/core/src/app.rs:292`, `crates/core/src/lib.rs:23`), so a handler extractor is invoked with `state: &AppContext<E, C>`, requiring `FromRequestParts<AppContext<E, C>>`, which is not implemented.

**Empirical proof:** compiling a route
```rust
router.route("/users/:id", axum::routing::get(
    |_: ValidatedParam<Id>, State(_): State<AppContext<(), ()>>| async {}
));
```
against `Router<AppContext<(), ()>>` yields:
```
expected struct `MethodRouter<AppContext<(), ()>>`
   found struct `MethodRouter<()>`
```
because `ValidatedParam<Id>` only satisfies `FromRequestParts<()>`, Axum infers handler state `()`, producing `MethodRouter<()>` which cannot be mounted on `Router<AppContext<(), ()>>`.

**Compounding:** the bundled marker types `matchers::{Number, Uuid, Slug}` (`routing.rs:155-162`) expose inherent `validate` methods but do **not** implement the `ParamMatcher` trait (`routing.rs:181-184`), so they cannot serve as `M` even in a `()`-state router. A user must define their own `ParamMatcher` type.

**What breaks:** the public `ValidatedParam` extractor is unusable as shipped in any real Rume app — any handler that tries to use it fails to compile. The feature is exported (`crates/core/src/lib.rs:25`, `crates/core/src/prelude.rs:14`) but dead on arrival.

---

### C2 (Major) — `ApplicationMode::Inspect` still serves HTTP traffic

**Evidence:** `crates/core/src/app.rs:47-53` documents `ApplicationMode::Inspect` as "Boot for inspection/tooling **without serving or side effects**." `crates/core/src/app.rs:404-451` (`run()`): the `if self.mode == ApplicationMode::Run` guard (line 421) only skips `on_ready` hooks; `axum::serve(...)` at line 433 runs **unconditionally** in both `Run` and `Inspect` modes, and the function blocks on it.

**What breaks:** a caller that selects `Inspect` expecting a boot-only, non-serving process gets a blocking HTTP server on the bound address instead. This violates the documented public contract of `RumeBuilder::mode()` and `ApplicationMode`.

---

### C3 (minor) — `Lifecycle::set` / `Availability::set` accept any `u8`; `state()` default arm silently maps invalid values to `Created`/`Unknown`

**Evidence:** `crates/core/src/app.rs:86-94` (`Lifecycle::state` default arm `_ => LifecycleState::Created`) and `crates/core/src/app.rs:140-148` (`Availability::state` default arm `_ => AvailabilityState::Unknown`). `set()` (`app.rs:96-98`, `app.rs:150-152`) stores `state as u8` with no range validation, so any `u8` (5, 255, etc.) can be written.

**What breaks:** an invalid stored value reads back as `Created`/`Unknown` rather than surfacing the corruption, masking state-machine bugs. Internally `set()` is `pub(crate)` and only called with valid enum values, so the risk is contained, but the default arm converts what should be an impossible state into a misleadingly-valid one.

---

### m1 (minor) — `Matcher::Number` doc ("digits only") does not match `u64`-bounded parsing

**Evidence:** `crates/core/src/routing.rs:47-48` doc comment: "Matches digits only, returns the string as-is." `crates/core/src/routing.rs:59-65` implementation: `value.parse::<u64>()`.

**What breaks:** an all-digits string that overflows `u64` (e.g. a 30-digit id) is rejected even though it is "digits only," and the returned value is the original string, not the parsed number (so "cast" in the module doc is a misnomer). Behavior is internally consistent, but the doc over-promises; a caller relying on "any digits" will see unexpected rejections for large ids.

---

### m2 (minor) — `RouteGroup::add` does not normalize path separators

**Evidence:** `crates/core/src/routing.rs:400-408` — `format!("{}{}", self.prefix, path)` with no slash handling.

**What breaks:** `prefix("/api/").add("/users")` produces `/api//users` (double slash), and `prefix("api").add("users")` produces `apiusers` (no leading slash). Routes may fail to match or produce unexpected paths. Axum's behavior with double slashes is not guaranteed to normalize.

---

### m3 (minor) — `RouteSpec::build()` registers in `ROUTE_REGISTRY` even when the route is never mounted

**Evidence:** `crates/core/src/routing.rs:353-363` — `register_route(info)` is called unconditionally inside `build()`, which consumes `self` and returns metadata. Registration is a build-time side effect, not a mount-time side effect.

**What breaks:** `registered_routes()` / `rume list:routes` can list routes that were built but never added to a router, producing stale registry entries. Noted in the review plan (`v01/plan.md` check #6). Low impact because registration is the documented purpose of `build()`, but the "build without mount" case pollutes the registry.

---

### m4 (minor) — `health_routes` `/health/ready` returns 503 for `Warning`, not just failures

**Evidence:** `crates/core/src/routing.rs:486-496` — `is_healthy` is `false` for any check whose status is not `HealthStatus::Ok`, so a `Warning` check makes `is_healthy == false`, and line 578-581 returns 503. The doc at `routing.rs:544-549` says ready returns "503 when **any check fails**"; a warning is not strictly a failure.

**What breaks:** a check that degrades to `Warning` (e.g. a slow-but-still-working dependency) triggers a 503 from readiness, potentially causing orchestration to remove a usable instance. This may be intentional (readiness = fully healthy), but it is stricter than the doc wording suggests.

---

## Positive notes

- `AppError::status_code()` (`crates/core/src/error.rs:39-48`) maps exactly as documented: `NotFound`→404, `BadRequest`→400, `Unauthorized`→401, `Forbidden`→403, `Validation`→422, `Internal`→500. `IntoResponse` bodies are consistent: `Validation` includes `details`; all others render `self.to_string()` in an `{"error": …}` envelope.
- `Validated<T>` (`crates/core/src/validation.rs:37-65`) correctly maps JSON/serde extraction failure → `AppError::BadRequest` (400) and `garde` validation failure → `AppError::Validation { details }` (422). The three `#[cfg(test)]` cases in `validation.rs:97-142` confirm 200/422/400 behavior with hand-derived expectations.
- `Matcher::Uuid` (`routing.rs:84-102`) and `Matcher::Slug` (`routing.rs:104-123`) validation logic is complete and correct for their documented formats (segment lengths, hex digits, leading/trailing/doubled separators).
- `Rejection::IntoResponse` (`routing.rs:599-610`) maps `Extraction`→400, `MissingParam`→400, `Validation`→422, matching the documented statuses.
- `run()` (`app.rs:404-451`) orders the lifecycle correctly: `Booted`/`Live` → (on_ready hooks) → `Ready`/`Ready` → serve → `Terminating`/`RefusingTraffic` → (on_shutdown hooks) → `Terminated`/`Unknown`, with `axum::serve` wrapped in `with_graceful_shutdown` for ctrl_c/SIGTERM.
- `ROUTE_REGISTRY` (`routing.rs:229-248`) handles lock poisoning gracefully (`if let Ok`, `unwrap_or_default`) rather than panicking.
- `health_routes` `/health/live` returns 503 only when availability is `Broken`, matching its "liveness, no dependency checks" contract.

---

## Verdict

**Fail — conditional.**

Two Major findings invalidate shipped public behavior as documented:

1. **C1** — `ValidatedParam<M>` is locked to `FromRequestParts<()>` and cannot compile against `Router<AppContext<E, C>>`, and its bundled markers do not implement `ParamMatcher`. The public extractor is unusable in a real app.
2. **C2** — `ApplicationMode::Inspect` still boots and serves HTTP traffic, contradicting its documented "without serving" contract.

Conditions to pass: fix C1 (make `ValidatedParam` state-generic or provide an `AppContext`-state extraction path, and implement `ParamMatcher` for the bundled markers) and fix C2 (guard `axum::serve`/the listening step behind `mode == Run`, or otherwise honor the Inspect contract). The four minor findings (C3, m1–m4) should be addressed but do not block the verdict on their own.

Once those are fixed, the surface is correct: status mappings, validation mapping, matcher logic, health semantics, and lifecycle ordering all match their docs and signatures.
