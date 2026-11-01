# DX/Usability Review — v01

**Report file:** `docs/reports/reviews/v01/dx-usability.md`
**Lens:** DX/Usability — is the API pleasant and intuitive to use?
**Code reviewed:**
- `rume/src/app.rs` — `AppContext::new()` (6 args), `Rume::builder(ctx)`, `RumeBuilder::new`, `ops_router`
- `rume/src/routing.rs` — `RouteSpec`, `RouteGroup`, `health_routes()` vs `ops_router()` naming, `ValidatedParam`
- `rume/src/prelude.rs` — re-exports
- `rume/src/lib.rs` — whole-crate re-exports
- `rume-cli/src/` — CLI commands (`new`, `env check`, `config check`, `doctor`, `generate`)
- `rume-cli/templates/` — generated project template

**Method:** whole-document, stage 1 (independent). All claims checked against the actual files above.

**Context from Stage 1:** Correctness C1 (`ValidatedParam` locked to `FromRequestParts<()>`, unusable with `AppContext`) is also a DX blocker — it is owned by the correctness lens and referenced here only for context. Architecture M1 (prelude too broad) and M2 (contradicted single entry point) are reviewed here from the usability angle.

---

## Summary

Rume's DX has a clear north star: one documented entry point (`Rume::builder(ctx)`), a typed `AppContext<E, C>`, explicit lifecycle/availability handles, and a CLI that scaffolds and validates. The onboarding path (`rume new` → `cargo run` → `rume doctor`) is thoughtfully scripted with next-step instructions. However, several real friction points mar first use: a 6-argument positional constructor, a second public constructor that contradicts the "only way" claim, two overlapping health-route APIs with confusingly similar names, a prelude that dumps whole third-party crates into the user's namespace, and a `rume new` scaffold that generates a non-buildable `Cargo.toml` (local path dependency) with no warning. The three Major findings should be fixed before v1; the minors are polish that compound the first-impression problem.

---

## Findings

### M1 — `AppContext::new()` takes 6 positional args of mixed types, making call sites fragile and hard to read

**Where:** `rume/src/app.rs:242-249` —
```rust
pub fn new(
    env: E,
    config: C,
    paths: ProjectPaths,
    availability: Availability,
    health_checks: crate::routing::HealthChecks,
) -> Self
```

**What it says:** Six positional parameters, two of which (`availability`, `health_checks`) are cloneable handles that are routinely passed by clone at the call site, while `env`/`config` are generics that may or may not need cloning depending on later use. The template call site at `rume-cli/templates/src/app.rs.tpl:19` reads:
```rust
let ctx = AppContext::new(env.clone(), config.clone(), paths, availability.clone(), health_checks);
```
Note `config.clone()` and `availability.clone()` are unnecessary there (neither is used again), while `env.clone()` is required because `env` is used again at `.bind((env.host, env.port))` on line 26. A reader cannot tell which clones are needed without tracking every later use.

**Why it's a problem:** Positional args of mixed types (`E`, `C`, `ProjectPaths`, `Availability`, `HealthChecks`) are easy to swap (env vs config) and impossible to self-document at the call site. An implementer must stop and check the signature to know argument order and whether to clone. Six args exceeds the comfortable small-arg threshold; this is the API a user touches on day one (it's in the `main` template).

**Suggested fix:** Replace the 6-arg constructor with a builder, e.g.
```rust
AppContext::builder()
    .env(env)
    .config(config)
    .paths(paths)
    .availability(availability)
    .health_checks(health_checks)
    .build()
```
and keep `new` as `pub(crate)` or remove it. Alternatively provide `AppContext::new_with_defaults(env, config, paths)` that constructs `availability`/`health_checks`/`lifecycle` internally, reserving the full constructor for advanced users. Either way the common call site should not require the caller to make manual clone decisions.

---

### M2 — `Rume::builder()` documented as the sole entry point, but `RumeBuilder::new()` is public and equivalent

**Where:** `rume/src/app.rs:274-281` (doc on `Rume::builder` — "This is the only way to build a Rume application.") and `rume/src/app.rs:307` (`pub fn new(ctx: AppContext<E, C>) -> Self`).

**What it says:** The doc comment on `Rume::builder` states it is "the only way to build a Rume application," yet `RumeBuilder::new` is `pub` and performs the identical construction. Both are exported from `lib.rs:18-21`.

**Why it's a problem:** A newcomer reads the docs, assumes one public construction path, then encounters a second public one. This forces an implementation-time decision about which API to standardize on, and which one tutorials and generated code should use. It directly contradicts the framework's stated "explicit, typed" and "single entry point" constraints (architecture M2). Generated code and examples use `Rume::builder`, so `RumeBuilder::new` is an undiscovered duplicate that invites inconsistency.

**Suggested fix:** Make `RumeBuilder::new` `pub(crate)` so `Rume::builder` is genuinely the only public entry point, or update the doc comment to acknowledge both public constructors and describe when to use each. Do not leave a public constructor that contradicts the documented single-entry-point story. (Same fix as architecture M2 — this is its usability dimension.)

---

### M3 — `health_routes()` and `ops_router()` are two overlapping health-endpoint APIs with confusingly similar names

**Where:** `rume/src/routing.rs:553-585` (`health_routes<E, C>() -> Router<AppContext<E, C>>`) and `rume/src/app.rs:536-580` (`ops_router<S>(router, availability) -> Router<S>`).

**What it says:** Both register `GET /health/live` and `GET /health/ready`. `health_routes` is generic over `AppContext<E, C>` and reads availability/health-checks from Axum `State`. `ops_router` is generic over any `S`, takes `Availability` directly (not via state), and its doc says "Ops endpoints for infrastructure probes." The template `rume-cli/templates/src/app.rs.tpl:22` uses `health_routes::<Env, Config>().merge(router)`.

**Why it's a problem:** A user sees two functions doing essentially the same thing (health endpoints) with different names (`health_routes` vs `ops_router`), different state-access patterns (`State<AppContext>` vs direct `Availability` capture), and different generic signatures. It is not obvious which to use, when, or what the distinction is. Merging both into one router would create route conflicts. The naming (`health_routes` vs `ops_router`) does not signal the difference clearly enough ("health" vs "ops" is a fine distinction, but the state model difference is the real one and is not signaled by the name).

**Suggested fix:** Merge into one canonical health-route function with a clear name, or clearly document the distinction: e.g. `health_routes` for Axum-state apps and `ops_router` for standalone routers without `AppContext` state — but prefer one API. At minimum, rename one to signal its distinct state model (e.g. `ops_router` → `health_routes_with_state` or `health_routes` → `app_health_routes`) and add a "which should I use?" note to both doc comments.

---

### m1 — Prelude re-exports whole third-party crates (`axum`, `serde`, `tokio`, `tracing`, `serde_json`) plus `axum::routing`, polluting the namespace

**Where:** `rume/src/prelude.rs:18-24` and `rume/src/lib.rs:30-33` (`pub use axum; pub use serde; pub use serde_json; pub use tokio; pub use tracing;`).

**What it says:** `rume::prelude::*` brings `axum`, `serde`, `tokio`, `tracing`, `serde_json`, `axum::routing`, `State`, `Path`, `Json` into scope. `lib.rs` additionally exposes `rume::axum`, `rume::serde`, `rume::tokio`, `rume::tracing` as first-class names.

**Why it's a problem:** The prelude dumps implementation dependencies — not Rume concepts — into the user's namespace. A user cannot tell at a glance what is Rume's own API versus a re-exported third-party crate, and `use rume::prelude::*` conflicts with their own `use axum::...` / `use serde::...` imports. Discoverability suffers: grepping `rume::` returns third-party types, not Rume types. This is architecture M1's usability dimension — the prelude is too broad.

**Suggested fix:** Narrow `prelude.rs` to Rume's own types plus the small extractor set users genuinely need (`State`, `Path`, `Json`). Remove whole-crate re-exports (`axum`, `serde`, `tokio`, `tracing`, `serde_json`, `axum::routing`). If users need `axum::routing::get`, they import `axum` directly. Re-exporting a whole crate should require a deliberate, documented reason per `70-code-rationale.md`.

---

### m2 — `rume generate handler` uses fragile string-marker codegen with silent degradation

**Where:** `rume-cli/src/commands/generate/handler.rs:62-90` — reads `src/routes.rs`, searches for the literal marker `// rume:routes`, and inserts a route line by string concatenation; if the marker is missing it prints a warning but continues without registering the route.

**What it says:**
```rust
} else if let Some(marker) = existing.find("    // rume:routes") {
    // ... insert route line before marker ...
} else {
    eprintln!(
        "warning: could not find route marker in {routes_path}; add the route manually"
    );
}
```

**Why it's a problem:** String-based codegen is fragile: formatting differences, marker removal, or manual edits to `routes.rs` silently break route registration. The "warning" path leaves a handler file on disk that is never routed, producing a confusing runtime 404 with no compile error and no hint that registration failed. The user must notice the warning and connect it to the missing route themselves.

**Suggested fix:** Treat a missing route marker as an error (exit non-zero) rather than a warning, or generate routes into a separate, included file so registration is compile-time checked. Either way, the user must not be left with a handler that silently isn't reachable.

---

### m3 — `rume new` scaffolds a `Cargo.toml` with a local path dependency and no user-facing warning

**Where:** `rume-cli/src/commands/new.rs:6` — `const FRAMEWORK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../rume");` and `rume-cli/templates/Cargo.toml.tpl:7` — `rume = { path = "{{framework_path}}" }`.

**What it says:** `rume new myapp` produces `rume = { path = "/.../rume" }` pointing at a local filesystem path that only exists on the developer's machine. The comment at `new.rs:5` acknowledges this ("Switch to a crates.io version before publishing") but the template still ships with it.

**Why it's a problem:** A newcomer following the quickstart (`rume new`, `cd`, `cargo run`) gets a build failure because the path dependency does not resolve on their machine. There is no hint at scaffold time that the dependency needs to be changed. This blocks the primary onboarding path. Acknowledged as pre-v1, but the absence of any user-facing warning makes it a DX defect, not just a known limitation.

**Suggested fix:** Default the template to a crates.io version pin (or a clearly-marked placeholder like `rume = { version = "0.1", ... }`) and only use the local path when an explicit `--dev`/local flag is passed. Add a post-scaffold note telling the user to verify/update the `rume` dependency before `cargo run`.

---

### m4 — Generated code imports via `rume::axum` / `rume::serde`, teaching the brittle re-export pattern

**Where:** `rume-cli/src/commands/generate/handler.rs:23-24` generates `use rume::axum::Json; use rume::serde::Serialize;` and templates use `rume::axum::routing::get` (`routes.rs.tpl:10-11`) and `rume::prelude::*`.

**What it says:** Generated handler code reaches through `rume::axum` and `rume::serde` — third-party crates re-exported as if they were Rume's own.

**Why it's a problem:** The scaffold teaches users to depend on `rume::axum` / `rume::serde`, which are re-exports that architecture M1 proposes removing. If those re-exports are removed, every generated project and every hand-written route using them breaks. It also couples user code to Rume's dependency choices. This is the prelude problem (m1) expressed in generated code.

**Suggested fix:** Generate imports from the crates directly (`use axum::Json; use serde::Serialize;`) and add `axum` and `serde` as direct dependencies in the template `Cargo.toml`, or at minimum add a doc note that `rume::axum` is a re-export that may be removed before v1.

---

### m5 — No machine-readable output (`--json`) for `doctor`, `env check`, `config check`

**Where:** `rume-cli/src/commands/doctor.rs:94-98` (`print_check` prints `[label] value`), `rume-cli/src/commands/env/check.rs:49` (`println!("  {key}={display}  (process env)")`), `rume-cli/src/commands/config/check.rs` (prints raw TOML text).

**What it says:** All CLI inspection output is human-readable text only. `doctor` prints `[label] value` lines; `env check` prints a table; `config check` prints raw merged TOML.

**Why it's a problem:** Output is human-only and hard to parse in scripts or CI. There is no `--format json` option, no structured schema, and no way to diff config/env across deploys. `config check` additionally dumps secrets plaintext (security M3 — that lens owns the redaction aspect; this finding is the absence of structured output).

**Suggested fix:** Add a `--format json` (or `--json`) flag to `doctor`, `env check`, and `config check` emitting structured output. Pair with secret redaction by default in `config check` (security M3).

---

## Positive notes

- `Rume::builder(ctx)` is a clear, single documented entry point; once D2/M2 is resolved (make `RumeBuilder::new` non-public), the "one way to build" story is genuinely honest.
- `AppContext<E, C>` is well-typed and explicit: env and config are separate type parameters, no hidden DI or scanning.
- `ProjectPaths` is a clean, well-named value object (`config_path`, `tmp_path`, `public_path`, `migrations_path`) that centralizes directory layout.
- `init_tracing()`, `Environment::from_env()`, and `Environment::in_development()`/`in_production()`/`in_test()` are sensible, well-named boot helpers.
- The CLI has good help text (`--help`), global `--no-color`/`--verbose` flags, clear error messages with hints (`require_rume_project` suggests `rume new`), and `rume generate` lists available generators when run without args and prompts for missing names.
- `rume new` gives clear, actionable next-step instructions (`cd`, `cp .env.example .env`, `cargo run`, `cargo watch`, `cargo test`, `rume doctor`, `rume --help`).
- `rume doctor` checks toolchain, environment files, configuration, `.env.example`, and port availability — a sensible onboarding validation.

---

## Verdict

**Not ready to proceed without a fix pass.**

The onboarding intent is strong and well-scripted, but three Major findings create real first-use friction: the 6-argument `AppContext::new()` (M1), the contradicted single entry point (M2), and the duplicate health-route APIs (M3). Each forces an implementer to stop and ask which API to use or how to call it. Combined with the minors (broad prelude, fragile codegen, non-buildable scaffold, brittle generated imports, no structured output), the DX is promising but not yet pleasant or intuitive.

Conditions to pass: fix M1 (builder or defaults for `AppContext`), M2 (make `RumeBuilder::new` `pub(crate)`), and M3 (one canonical health-route API). Address m1–m5 in the same pass. With those fixes, the `rume new` → `cargo run` → `rume doctor` path would be solid and the framework's explicit, typed character would shine through at the API level.