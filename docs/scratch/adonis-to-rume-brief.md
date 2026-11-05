# AdonisJS → Rume concept brief

Discussion-only. No implementation decisions are made here; this is a
brainstorming artifact for the next planning phase.

Source constraints: `.opencode/rules/60-architecture.md`,
`docs/specs/vision/README.md` (§7 v1 contract, §9 borrowing policy,
§11 pre-v1 rules). Rume is pre-v1; everything outside the v1 contract is
experimental and may break.

---

## Already adopted from AdonisJS v7

| Adopted | AdonisJS inspiration | How it fits Rume | Status |
|---|---|---|---|
| Centralized error model | `AppError` + `ExceptionHandler` handle/report split | `AppError` enum with `IntoResponse` (client) and server-side logging (report) inside `into_response`; RFC 7807-shaped bodies | Implemented in `crates/core/src/error.rs` |
| Route parameter matchers | Adonis route param validation (number/uuid/slug patterns) | `Matcher::Number`/`Uuid`/`Slug` + `ValidatedParam` extractor; never echoes raw input in errors | Implemented in `crates/core/src/routing.rs` |
| Route groups with prefix normalization | Route groups / prefix grouping | `RouteGroup::prefix`/`as` with single-slash `join_path` | Implemented in `crates/core/src/routing.rs` |
| Direct crate imports in generated code | Not Adonis-specific; DX cleanliness | Generated code uses `axum::Json`, `serde::Serialize` etc. rather than whole-crate re-exports | Implemented in CLI templates |
| Health checks (`/health/live`, `/health/ready`) | Adonis health package (lighter touch than Spring Actuator) | `HealthCheck` trait + `HealthChecks` registry; readiness runs registered checks, liveness is process-only | Implemented in `crates/core/src/routing.rs` |
| Typed env (casting/validation) | Adonis `Env` with schema rules | `Env` deserialized from layered `.env` into typed Rust structs at boot; secrets in `SecretBox`, redacted | Implemented in `crates/core/src/env.rs` |
| CLI generators | Adonis `ace` (`make:controller`, etc.) | `rume new`/`dev`/`test`/`env check`/`doctor`/`generate handler` | Implemented in `rume-cli` |

---

## Recommended to adopt next

Each entry: AdonisJS inspiration → fit with Rume's constraints → pre-v1? → cost / maintenance burden.

### 1. Global exception handler registration (Adonis `ExceptionHandler`)

**Inspiration.** Adonis has a single `ExceptionHandler` class with `handle(error, response)` (client response) and `report(error, logger)` (server logging). Rume already splits client response (`IntoResponse`) from server logging inside `AppError::into_response`, but the logic is baked into the enum.

**Fit.** A user-registered `ExceptionHandler` trait (or closure) invoked by the framework for errors that escape handlers, configured explicitly on `Rume::builder()`. No hidden AOP — no annotation-driven error interception; the handler is a builder-level dependency constructed in code. Keeps the `handle`/`report` separation but makes it pluggable and explicit.

**Pre-v1?** Yes. Error handling is in the v1 contract (§7.4), and this is additive to `AppError`.

**Cost / burden.** Low. One trait + one builder field + a call site in the Axum error handling layer. Maintenance: small surface, easy to test hermetically.

### 2. Richer validation rules / schema validation layer (Adonis `Validator`)

**Inspiration.** Adonis `Validator` with compile-time-ish schema objects and reusable rules (`required`, `email`, `unique`, `exists`, `date`, `alpha`, confirmation, etc.) plus custom rules.

**Fit.** Rume already has `Validated<T>` backed by `garde`. The natural extension is a rule catalog + named schemas (e.g. `schema::create_user()`) that produce `garde`-compatible validators, keeping errors as structured problem details. Explicit: schemas are Rust values constructed at boot or inline; no stringly rule keys. Could live in core (garde is already a dependency) or a `rume-validation` crate.

**Pre-v1?** Yes — validation is in the v1 contract. Worth one phase to round out error→validation integration.

**Cost / burden.** Low-medium. A rules module is pure Rust and easily tested; the risk is rule creep (don't port every Adonis rule — port only what the hero workflow needs).

### 3. Resourceful routing / route controllers (Adonis `Route.resource`)

**Inspiration.** Adonis `Route.resource('users', 'UserController')` maps the seven RESTful actions (index/create/store/show/edit/update/destroy) to controller methods.

**Fit.** An explicit `Resource` builder that takes named handler functions and emits standard `RouteSpec`s under a `RouteGroup` prefix. No reflection, no controller-class discovery, no naming conventions that magic routes into existence — the user lists handlers explicitly. The URL names (`users.index`, `users.store`, …) follow a documented convention but are opt-in.

**Pre-v1?** Yes — HTTP layer is in the v1 contract; resource routing is a DX convenience on top of `RouteSpec`/`RouteGroup`.

**Cost / burden.** Low. Thin layer over existing routing. Maintenance: naming convention docs + tests that generated routes match the convention.

### 4. Named middleware / middleware groups (Adonis named middleware)

**Inspiration.** Adonis registers middleware by name (`auth`, `throttle`, `bodyparser`) and applies groups by alias.

**Fit.** Rume already uses Tower middleware via the builder. The addition: a named registry (`middleware("auth", AuthLayer)`) and groups (`group("api", [auth, throttle])`) applied to `RouteGroup`s by name. All registrations are explicit builder calls; no annotation or attribute-driven middleware. Stays Tower-compatible.

**Pre-v1?** Yes — middleware is in the HTTP layer of the v1 contract.

**Cost / burden.** Low-medium. A registry + group type; tests for group composition and ordering. Risk: scope creep into auth-specific middleware (keep auth out of core; see `rume-auth`).

### 5. Events / subscribers (Adonis `Event`)

**Inspiration.** Adonis typed events with subscriber classes (`on('user.created', …)`).

**Fit.** An explicit `Events` registry: users define event types and register `Subscriber` impls or async handler closures at builder time. No global event bus discovered by scanning; the registry is a plain value threaded through `AppContext` or passed explicitly. Fits "transactions and lifecycle hooks are visible, not proxied."

**Pre-v1?** Yes-ish — not in the v1 contract list, but small and additive; could ship pre-v1 as experimental or wait for v1.x.

**Cost / burden.** Low. One trait + registry; the maintenance risk is tempting to make it distributed/actor-like — resist.

### 6. `make:*` CLI generators beyond handlers (Adonis `ace`)

**Inspiration.** Adonis `ace make:controller`, `make:middleware`, `make:validator`, etc.

**Fit.** `rume-cli` already scaffolds projects and handlers. Extend generators to middleware, validators, and (later) subscribers/templates. Templates are real files, overridable, must compile.

**Pre-v1?** Yes — CLI is in the v1 contract.

**Cost / burden.** Low per generator; main burden is keeping templates in sync with API changes (already a known discipline: "Generated code must build").

### 7. Query builder abstraction (Adonis Lucid query builder — NOT Lucid ORM)

**Inspiration.** Lucid's fluent query builder, without the ORM/active-record commitment.

**Fit.** Optional `rume-db` crate with a SQLx-compatible query builder helper. Explicit: user constructs the builder and passes a pool; no implicit transactions, no model magic. No mandatory ORM — entirely optional and crate-boundary-respecting.

**Pre-v1?** No. Vision §7.2: v1 does not require auth, mail, jobs, or storage; DB helpers are `rume-db`, a v1.x optional crate.

**Cost / burden.** Medium-high. A query builder is a large surface to maintain and a strong magnet for scope creep ("just add relations…"). Defer until a real user need is demonstrated.

### 8. View / template rendering abstraction (Adonis `view` + Edge)

**Inspiration.** Adonis `view.render('welcome', data)` with Edge templates.

**Fit.** Optional `rume-views` crate with a `Renderer` trait and engine adapters (askama/minijinja). Not built into core; no mandatory template engine; no magic template discovery.

**Pre-v1?** No — views are not in the v1 contract (Rume targets APIs/SaaS first). Defer to v1.x optional crate.

**Cost / burden.** Medium. Templates add a second compile step/engine dependency; keep out of core until requested.

---

## Defer / reject with reasoning

| Idea | Decision | Reasoning |
|---|---|---|
| Lucid-style mandatory ORM / active record | **Reject** | Explicit refusal: "No mandatory ORM or database" (60-architecture §3 refusals; vision §9). |
| Service providers / runtime DI container | **Reject** | Refusals: no runtime DI, no classpath/module scanning. Adonis providers map to Rume's explicit `Rume::builder()` assembly, not a container. |
| `@Middleware` / `@Transactional`-style annotations | **Reject** | Refusal: no hidden AOP. Middleware and transactions must be explicit Tower layers / visible transaction boundaries. |
| `config.get::<T>("app.url")` stringly-typed config access | **Reject** | Typed config validated at boot; no runtime type erasure (vision §6.2.4). |
| Built-in template engine with magic conventions | **Reject for core** | OK as an optional crate (`rume-views`); core stays template-agnostic. |
| Ace-style interactive CLI prompts | **Defer** | CLI exists; prompts add terminal/UX complexity before the non-interactive hero workflow is proven. |
| Global middleware applied by convention (folder scanning) | **Reject** | No scanning; middleware registered explicitly at builder time. |

---

## Open questions for the user

1. **Exception handler:** Is a pluggable `ExceptionHandler` trait worth the indirection over today's `AppError::IntoResponse`? Or should we keep error handling as an enum and only add logging hooks?
2. **Validation:** Keep validation as `garde`-backed `Validated<T>` in core, or split into a `rume-validation` crate with an Adonis-style rule catalog?
3. **Resource routing:** Generate route specs from handler names (framework-side convention), or generate handler scaffolding via `rume generate resource` (CLI-side)?
4. **Middleware groups:** Tower layer composition at builder time vs. named groups applied by route — which matches how users actually organize auth/rate-limiting?
5. **Events:** Core or optional crate? Is there an in-app event need today, or is this speculative?
6. **Query builder:** Should `rume-db` be the first optional crate after v1, or does the team want SQLx/ Diesel left entirely to users?
