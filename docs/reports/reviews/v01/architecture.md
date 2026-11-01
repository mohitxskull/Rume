# Architecture Review — v01

## Summary

Rume's architecture is largely consistent with its stated constraints: an explicit, typed application shell with a real Env/Config split, no runtime DI container, no classpath/module scanning, no hidden AOP, no mandatory ORM, and no hot-reload. The `AppContext<E, C>` generic carries env and config as separate typed parameters, `Rume::builder(ctx)` is the single documented entry point, lifecycle and availability are visible atomic handles rather than proxied hooks, and the `rume-cli` crate depends on `rume`, not the reverse. The main defects are a public API surface that leaks unrelated third-party crates (lib.rs and prelude), a documented "single entry point" that is contradicted by a public second constructor, a tracing initializer that swallows its failure, and lifecycle state coupled to enum discriminant order.

## Findings

### M1 — Public API surface leaks unrelated third-party crates; prelude is too broad

**Where** — `rume/src/lib.rs:30–33`; `rume/src/prelude.rs:18–24`.

**What it says** — `lib.rs` re-exports whole third-party crates as part of rume's public API:

```rust
pub use axum;
pub use serde;
pub use serde_json;
pub use tokio;
pub use tracing;
```

and `prelude.rs` re-exports `axum`, `serde`, `serde_json`, `tokio`, plus axum extractors (`State`, `Path`, `Json`, `routing`).

**Why it's a problem** — This makes `rume::tokio`, `rume::axum`, `rume::serde`, etc. first-class names in rume's public API, coupling consumers to rume's dependency and version choices and polluting their namespaces with implementation details. It contradicts the "explicit, typed" constraint by surfacing unrelated third-party crates as if they were rume's own, and it r
emains a maintenance burden every time rume upgrades or drops a dependency. The charter question "is [the prelude] too broad?" is answered: yes.

**Suggested fix** — Remove the whole-crate re-exports from `lib.rs` (lines 30–33). Narrow `prelude.rs` to rume's own types plus the small set of extractors users genuinely need (`State`, `Path`, `Json`), or drop the prelude entirely and require direct imports. Re-exporting a whole crate should require a deliberate, documented reason per `70-code-rationale.md`.

### M2 — `Rume::builder` documented as the sole entry point, but `RumeBuilder::new` is public and equivalent

**Where** — `rume/src/app.rs:276–282` (doc on `builder`); `rume/src/app.rs:307` (`RumeBuilder::new`).

**What it says** — The doc comment on `Rume::builder` states: "This is the only way to build a Rume application." Yet `RumeBuilder::new(ctx)` (line 307) is `pub` and performs the same construction, so there are two public paths to the same builder.

**Why it's a problem** — This directly undermines the chartered "single `builder(ctx)` entry point — is it truly explicit?" question. An implementer reading the docs assumes one public construction path, then finds a second public one, which is an inconsistency that forces an implementation-time decision about which API to standardize on.

**Suggested fix** — Either make `RumeBuilder::new` `pub(crate)` so `Rume::builder` is genuinely the only public entry point, or update the doc comment to acknowledge both public constructors and describe when to use each. Do not leave a public constructor that contradicts the documented single-entry-point story.

### m1 — `init_tracing()` silently discards subscriber initialization failure

**Where** — `rume/src/app.rs:509–515`.

**What it says** —

```rust
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}
```

The `try_init()` result is discarded with `let _ =`.

**Why it's a problem** — If the tracing subscriber cannot be installed (e.g. a conflicting global subscriber already exists), production logging setup fails silently with no observable signal. This conflicts with the production-readiness constraint: an operator has no way to know that logging is absent.

**Suggested fix** — Return `Result<(), tracing::subscriber::SetGlobalDefaultError>` (or at minimum log the failure through a fallback mechanism) so a tracing init failure is visible rather than swallowed. Document the behaviour at boot.

### m2 — Lifecycle/Availability atomic state is coupled to enum discriminant declaration order

**Where** — `rume/src/app.rs:86–97` (`Lifecycle::state`); `rume/src/app.rs:140–151` (`Availability::state`).

**What it says** — Both `state()` methods load a raw `u8` from an `AtomicU8` and match on literal values `1..=4`, defaulting to the first variant, relying on the enum variants being laid out in declaration order (`Created` = 0, `Booted` = 1, `Ready` = 2, ...).

**Why it's a problem** — Reordering either enum silently breaks state transitions: the numeric match arms would no longer correspond to the intended variants. It is a latent correctness bug hiding behind an implicit convention, and neither enum carries `#[repr(u8)]` or explicit discriminants to pin the layout.

**Suggested fix** — Add `#[repr(u8)]` with explicit discriminant values to `LifecycleState` and `AvailabilityState`, or store the state as the enum directly rather than a raw `u8`. This makes the coupling explicit and failure-proof against reordering.

## Positive notes

- The `Env`/`Config` split is real and clean: `rume/src/env.rs` owns `.env`/process-env loading with explicit precedence, and `rume/src/config.rs` owns declarative TOML under `config/`; both share a single env-name resolver (`env::resolve_env_name`, `env.rs:94`) rather than duplicating it.
- `AppContext<E, C>` passes env and config as separate typed generics (`app.rs:218–235`); dependencies are constructed in code, not discovered by scanning.
- `Rume::builder(ctx)` + `RumeBuilder` are an explicit builder API; lifecycle hooks (`on_ready`/`on_shutdown`, `app.rs:358–376`) are visible, not proxied/AOP.
- `#![forbid(unsafe_code)]` in `lib.rs:7`; secrets are modeled with `secrecy` and a `redact()` helper exists (`env.rs:257`).
- The crate-boundary rule is respected: `rume` is intentionally small (`lib.rs:1–5`), optional concerns live behind features (`schemars`, `config.rs:93`), and `rume-cli` depends on `rume` (`rume-cli/Cargo.toml:14`), not the reverse.
- All six architecture refusals (no DI container, no scanning, no hidden AOP, no mandatory ORM, no hot-reload, explicit/typed) appear to be honored in the reviewed files.

## Verdict

The architecture is sound and ready to proceed, subject to fixing M1 and M2 (the public-API leakage and the contradicted single-entry-point claim) before v1. m1 and m2 are low-risk polish items that should be fixed in the same pass but do not block implementation. With those fixes, Rume's crate boundaries, explicitness, and Env/Config split will match its stated constraints.
