# Rume — Vision

> Status: pre-v1 draft. This is a design reference: it records intent and
> trade-offs. It is not a plan, not a specification of current behavior, and
> not a promise of delivery.

## 1. Executive summary

Rume is a public Rust web framework for teams that want the coherence of a
full-stack framework without surrendering their database, configuration, or
runtime to it.

The thesis:

> **Rume is an explicit, typed, production-ready application shell for Axum.**
>
> It gives Adonis-style application structure, Spring Boot-style operational
> maturity, and Rust's compile-time guarantees — with no runtime container,
> no classpath magic, and no ORM lock-in.

Rume is not an attempt to out-feature [Loco](https://loco.rs/). Loco is already
"Rails in Rust" and is good at it. Rume's bet is that a specific kind of team
wants something different: a framework that is **explicit where Loco is magical,
typed where Loco is templated, and modular where Loco is monolithic.**

The first proof of that thesis is not an ORM or a mailer. It is the thing most
frameworks get wrong and most teams rebuild by hand: **configuration and
environment handling.** Typed env, layered `.env`, secret redaction, provenance,
and `rume env check`.

## 2. The problem

Rust web development today has a predictable shape:

1. Pick Axum, Actix, or Rocket.
2. Assemble config, env, logging, auth, validation, DB, mail, jobs, testing,
   and deployment by hand.
3. Rebuild the same glue in every project.
4. Discover the gaps (secret handling, env validation, test harnesses) in
   production.

Loco solves this by being Rails: batteries included, opinionated, and cohesive.
That is the right answer for many teams. But it comes with commitments that
some teams will not accept:

- SeaORM as the database story.
- YAML config rendered through a template pass before parsing.
- Secrets as plain `String` fields with no redaction.
- Env layering left to the deployment platform.
- A fixed framework `Config` struct with app settings as free-form JSON.

Rume exists for the team that wants the framework's coherence but not those
commitments. It is closer to **Adonis** than to Rails, and closer to **Spring
Boot's operational surface** than to Spring's runtime machinery.

## 3. Target user

### 3.1 Primary user

A small team (1-8 developers) building an API or SaaS in Rust that:

- Likes Rust's type system and wants to lean on it.
- Wants a framework's structure, generators, and defaults.
- Does not want to hand the framework their database choice.
- Cares about production operations (health, metrics, config inspection).
- Is willing to be explicit in exchange for debuggability.

### 3.2 Not the primary user

- Teams that want Rails-style magic and convention-over-configuration at all
  costs. They should use Loco.
- Teams that want a zero-config "just run it" experience. Rume is explicit by
  design.
- Teams that want Spring Security's full feature surface. Rume will offer a
  coherent, smaller auth package, not a clone.
- Solo founders optimizing for shipping speed above all else. Loco is likely a
  better fit today.

## 4. Competitive landscape

### 4.1 Loco

Strengths: mature (v1.2), Axum-based, generators, auth, mail, jobs, cache,
storage, testing, CLI, good docs.

Weaknesses (from Rume's perspective):

- Rails-shaped: conventions and magic over explicit wiring.
- SeaORM-shaped: the database story is not optional in practice.
- Config is YAML + `<%= get_env(...) %>` templating; secrets are plain strings.
- No dedicated secret type or redaction.
- No `.env` layering inside the framework.
- App settings are free-form `serde_json::Value`.
- Fixed framework `Config` struct.

Rume's relationship to Loco: **complement and alternative**, not enemy. A team
that outgrows Loco's commitments is Rume's natural user.

### 4.2 Axum + manual assembly

Strengths: maximum control, ecosystem standard, no framework lock-in.

Weaknesses: every project rebuilds config, env, validation, errors, testing,
and ops. That tax is the problem Rume exists to remove.

### 4.3 AdonisJS

Strengths: excellent DX, explicit lifecycle, service providers, typed env
validation, first-party packages, strong conventions.

Weaknesses for Rust: TypeScript runtime, no compile-time guarantees, dynamic
config access via `config.get<T>()`.

Rume borrows Adonis's **application shape and DX**, but replaces runtime
dynamism with compile-time types.

### 4.4 Spring Boot

Strengths: externalized config, profiles, starters/BOM, actuator, test slices,
validation, problem details, operational maturity.

Weaknesses: runtime DI, classpath scanning, annotation magic, heavy startup,
hidden AOP.

Rume borrows Spring Boot's **operational surface and product shape**, not its
runtime machinery.

### 4.5 Micronaut / Quarkus

Strengths: compile-time DI, AOT, build-time configuration, dev mode, native
image.

Weaknesses: complex build-time machinery, steep learning curve.

Rume borrows their **compile-time philosophy** where it fits Rust, but avoids
their build-time complexity.

### 4.6 Summary

| Axis | Loco | Axum+manual | Adonis | Spring Boot | Rume (target) |
|---|---|---|---|---|---|
| Coherence | High | Low | High | High | High |
| Explicitness | Medium | High | Medium-High | Low-Medium | High |
| Type safety | Medium | High | Low (TS) | Medium | High |
| DB freedom | Low | High | Medium | Medium | High |
| Ops maturity | Medium | Low | Medium | High | High |
| Magic | Medium-High | Low | Medium | High | Low |

## 5. Thesis and positioning

Rume's one-line positioning:

> **The explicit, typed, production-ready application shell for Axum.**

Three commitments:

1. **Explicit.** Dependencies are constructed in code, not discovered by
   scanning. Configuration is typed, not templated. Transactions are visible,
   not proxied.
2. **Typed.** Env, config, requests, responses, and errors are Rust types
   validated at the boundary. No stringly keys in application code.
3. **Production-ready.** Health, readiness, metrics, tracing, config inspection,
   and a doctor command are part of the framework, not afterthoughts.

Three refusals:

1. No runtime dependency injection container.
2. No classpath or module scanning.
3. No mandatory ORM or database.

## 6. Product shape

### 6.1 Application model

Rume applications are built explicitly:

```rust
let app = Rume::builder()
    .with_config(Config::load()?)
    .with_state(AppState { db: pool })
    .with_routes(routes())
    .with_middleware(stack)
    .build()?;

app.run().await?;
```

There is no hidden service locator. State is a typed struct. Routes are
functions. Middleware is Tower. The builder is the single place where the
application is assembled.

Lifecycle hooks (startup, shutdown, health) are explicit methods on the builder
or on a `Hooks` trait implemented by the app.

### 6.2 Configuration and environment

This is Rume's first-class feature and its first proof of concept.

#### 6.2.1 Typed env

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct Env {
    pub host: IpAddr,
    pub port: u16,
    pub app_key: SecretBox<String>,
    pub database_url: Url,
    #[serde(default)]
    pub log_level: LogLevel,
}
```

- Validated at boot. Missing or invalid variables produce clear errors naming
  the variable, expected type, and source.
- Secrets are wrapped in `SecretBox`/`SecretString` and redacted in logs and
  `Display`.
- Optional variables use `Option<T>` or `#[serde(default)]`.

#### 6.2.2 Layered `.env`

Files, highest to lowest priority (for `RUME_ENV=development`):

1. `.env.development.local`
2. `.env.local`
3. `.env.development`
4. `.env`

Rules:

- Process environment always wins over files.
- `.env.local` is not loaded when `RUME_ENV=test` or `testing`.
- Empty values count as defined and override lower-priority files.
- Interpolation: `$VAR` and `${VAR}`, with `\` escaping.
- `file:` prefix resolves file contents, with validation.
- Custom prefixes via `Env::define_identifier`.

#### 6.2.3 Config files

Config lives in `config/` as typed TOML, not executable code:

```toml
# config/app.toml
[app]
name = "My App"
url = "http://localhost:3333"

[server]
binding = "0.0.0.0"
port = 3333
```

- Loaded and validated into a typed `Config` struct at boot.
- Environment-specific overrides: `config/app.production.toml`.
- Deep-merge of base + local, like Loco, but without templating.
- Env values used explicitly at load time, not via template strings.

#### 6.2.4 Config access

```rust
let url = app.config.app.url.clone();
```

No `config.get::<T>("app.url")`. No stringly keys. No runtime type erasure.

#### 6.2.5 `rume env check`

Prints resolved values, sources, and redacted secrets. Useful for debugging
"why is this value what it is."

#### 6.2.6 Config schema

Generate a JSON schema for `config/*.toml` and a `.env.example` from the typed
structs. This gives editor autocomplete and documentation for free.

#### 6.2.7 Design rationale: env vs config, TOML vs Rust

**Env vs config.** `Env` holds deployment-specific values and secrets (`HOST`,
`PORT`, `DATABASE_URL`, `APP_KEY`). `Config` holds application structure and
behavior (`app.toml`: app name, server binding, pool sizes, feature flags).
The split keeps secrets out of committed files and lets ops change behavior
without recompiling. They compose: config files are validated into typed Rust
structs, and env values are used explicitly in Rust code rather than via
template strings.

**Environment-specific files.** `.env`, `.env.development`, `.env.local`, and
`config/app.toml`, `config/app.development.toml`, `config/app.local.toml` are
layered overrides, not separate apps. This mirrors Adonis/Spring profiles and
avoids `if env == "prod"` branches in code.

**Why TOML instead of Rust files.** The schema and types are Rust (compile-time
checked). The values are data (TOML) so ops can change them without a rebuild,
review them as diffs, and keep secrets out of source. Type safety is preserved
at the boundary: TOML is deserialized into Rust structs at boot, and bad values
fail startup with clear errors. Rust-defined defaults may be added later as an
additive override, but TOML remains the primary source.

### 6.3 HTTP layer

Rume builds on Axum. It does not reimplement HTTP.

- Public API is Rume-owned types where it improves DX; Axum types are re-exported
  as escape hatches.
- Extractors for JSON, form, query, path, validated payloads.
- Central error type (`AppError`) implementing `IntoResponse`, defaulting to RFC
  7807 problem details.
- Middleware via Tower, with a curated catalog of common middleware.
- WebSocket support via external Axum-compatible crates, not built-in.

### 6.4 Validation

- `Validated<Json<T>>`, `Validated<Query<T>>`, etc.
- Powered by `garde` or `validator`.
- Errors map to structured problem details, not ad-hoc strings.

### 6.5 Error model

- One `AppError` enum with variants for common cases.
- Automatic HTTP status mapping.
- Structured error body (RFC 7807).
- `?`-friendly: handlers return `Result<Response, AppError>`.

### 6.6 Testing

First-class test harness:

```rust
let app = TestApp::http().build();
let res = app.get("/health").await;
assert_eq!(res.status(), 200);

let app = TestApp::with_db().build().await;
// seeded DB, automatic cleanup on drop
```

- HTTP-only tests do not boot a database.
- DB-backed tests get automatic per-test cleanup.
- Fixtures and snapshot testing supported.
- Test app builder mirrors the real app builder.

### 6.7 CLI and dev loop

```sh
rume new <name>        # scaffold a project
rume dev               # run with watch/restart
rume test              # run tests
rume env check         # inspect resolved env/config
rume doctor            # validate DB, queue, deps, config
rume generate <kind>   # scaffold controllers, models, etc.
```

- `rume dev` uses watch + restart. No promise of JVM-style hot swap.
- Generators are templates that can be overridden.

### 6.8 Observability

- `tracing` integration out of the box.
- Metrics via `metrics` crate, Prometheus endpoint.
- OpenTelemetry support as an optional feature.
- `rume doctor` validates connectivity and configuration.

### 6.9 Optional packages (roadmap)

Official crates, each optional:

- `rume-auth` — sessions, JWT, password hashing, guards.
- `rume-db` — SQLx/Diesel helpers, migrations, test fixtures.
- `rume-mail` — SMTP mailer, templates.
- `rume-jobs` — background jobs, scheduling.
- `rume-storage` — file storage (local, S3).
- `rume-cache` — cache abstraction.

These are not part of the v1 contract but follow the same design principles.

### 6.10 Extensibility

- Tower middleware for HTTP concerns.
- Traits for pluggable backends (DB, cache, queue, mail).
- Feature flags for optional functionality.
- No runtime plugin system.

## 7. v1 contract

### 7.1 Stable before 1.0

1. App bootstrap and lifecycle.
2. Typed config/env.
3. HTTP layer on Axum.
4. Validation.
5. Testing harness.
6. CLI (`new`, `dev`, `test`, `env check`, `doctor`).
7. Observability hooks.

### 7.2 Graduation criteria

v1 ships when:

- The hero workflow is demonstrably better than manual Axum assembly.
- The hero workflow is demonstrably more explicit than Loco for the target
  user.
- The core crate compiles with minimal dependencies.
- The test harness supports both HTTP-only and DB-backed tests.
- `rume doctor` catches common misconfigurations.
- Documentation covers the hero workflow end to end.

v1 does **not** require auth, mail, jobs, or storage. Those are v1.x features.

## 8. Crate layout

```
rume/                  # workspace root
├── rume/              # core library
├── rume-cli/          # binary
├── rume-macros/       # proc macros (if needed)
├── rume-auth/         # optional
├── rume-db/           # optional
├── rume-mail/         # optional
└── rume-jobs/         # optional
```

### 8.1 Dependency rules

- Core depends on Axum, Tokio, Serde, Tracing, and a small set of utilities.
- Core does not depend on any optional heavy crate.
- Optional crates depend on core, never the reverse.
- Version pins are exact (no ranges).
- Workspace-level dependency management for consistency.

## 9. Borrowing policy

| Source | Borrow | Do not borrow |
|---|---|---|
| Adonis | App skeleton, config/env behavior, generators, lifecycle | Runtime dynamism, `config.get<T>()` |
| Spring Boot | Starters, actuator, test slices, problem details, config schema | Runtime DI, classpath scanning, AOP |
| Micronaut/Quarkus | Compile-time wiring, typed config, build-time augmentation | Build-time complexity, annotation processors |
| Loco | Lessons in what not to do | YAML templating, ORM lock-in, fixed config struct |
| Axum/Tower | Everything (substrate) | — |

## 10. Non-goals

- Feature parity with Loco.
- A runtime service container.
- Classpath or module scanning.
- Hidden transaction boundaries.
- A mandatory ORM.
- JVM-style hot reload.
- A runtime plugin system.

## 11. Pre-v1 rules

- Public framework, but pre-1.0 APIs may break freely.
- Borrow aggressively and rewrite as needed.
- Every feature must justify its maintenance cost.
- No feature is added without a test in the hero workflow.
- The v1 contract is frozen; everything else is experimental.

## 12. Hero workflow and demo script

```sh
# Install
cargo install rume-cli

# Create
rume new blog-api
cd blog-api

# Configure
cp .env.example .env
# edit .env and config/app.toml

# Run
rume dev
# -> server on http://localhost:3333
# -> GET /health returns {"status":"ok"}

# Add a route
rume generate controller posts
# edit src/controllers/posts.rs

# Test
rume test
# -> HTTP tests pass
# -> DB tests pass (if configured)

# Inspect
rume env check
rume doctor

# Deploy
docker build -t blog-api .
```

Success means: a developer new to Rume can go from `rume new` to a deployed
API with Postgres, auth, typed config, and tests in under 30 minutes, and can
swap the database layer without rewriting application code.

## 13. Success metrics

- Time from `rume new` to first deployed endpoint: < 30 minutes.
- Lines of configuration for a basic API: < 20.
- Number of concepts to learn before first route: < 5.
- `rume doctor` catches > 80% of common misconfigurations.
- Core crate compile time: competitive with Axum alone.
- Zero stringly-typed config access in generated code.

## 14. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Loco copies the good ideas | Move fast on the env/config proof; it's the hardest to retrofit |
| "Modular" becomes "assemble it yourself" | Opinionated default path via `rume new` and presets |
| Maintenance burden of a framework | Keep core small; push optional features to separate crates |
| Rust compile times | Minimal default features; feature-gate heavy deps |
| Scope creep before v1 | Frozen v1 contract; experimental label on everything else |
| No community | Documentation and hero workflow first; community second |

## 15. Open questions

1. Which Java framework is the primary reference: Spring Boot, or
   Quarkus/Micronaut?
2. Is auth part of the v1 contract or an official crate?
3. Which database story ships first: SQLx helpers, SeaORM integration, or both?
4. What is the first concrete pain point the hero workflow must remove?
5. Should `rume` be the crate name, or is there a better name?
6. How much of Axum's API should Rume re-export vs. wrap?

## 16. Decision log

- 2026-10-25: CLI is organized as `cli.rs` plus a `commands/` module tree.
  Nested groups (`env`, `config`) are folders; leaf commands are files.
- 2026-10-25: `rume env check` hides process-environment variables by default
  and shows them only with `--all`, to keep output focused on project config.
- 2026-10-25: `lab/` is gitignored for local experiments and generated projects.
- 2026-10-25: `rume new` templates are real files under `rume-cli/templates/`,
  rendered with a small in-house placeholder renderer (no template engine
  dependency).
- 2026-10-25: Env and config are separate systems: env holds deployment values
  and secrets; config holds structured TOML validated into Rust types.
- 2026-10-25: v1 contract is frozen; everything outside it is experimental.

## 17. Revision history

- 2026-10-25: Initial vision draft.
