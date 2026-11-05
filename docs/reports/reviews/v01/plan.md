# Lens plan — Rume whole-document review (v01)

Review mode: **whole-document** (confirmed). This is the first review of the
repository, staged under `docs/reports/reviews/v01/`. The plan governs the
review only; it never outranks the code (`.opencode/rules/20-doc-tree.md`).

## Review scope

**In scope** (the whole document, i.e. the shipped source surface):

| Crate | Files |
|---|---|
| `rume` (core lib) | `crates/core/src/lib.rs`, `prelude.rs`, `app.rs`, `config.rs`, `env.rs`, `error.rs`, `routing.rs`, `testing.rs`, `validation.rs` |
| `rume-cli` (binary) | `crates/cli/src/main.rs`, `cli.rs`, `commands/mod.rs`, `commands/env/mod.rs`, `commands/env/check.rs`, `commands/config/mod.rs`, `commands/config/check.rs`, `commands/doctor.rs`, `commands/new.rs`, `commands/codegen/mod.rs`, `commands/generate/mod.rs` (+ `generate/{handler,model,service,middleware}.rs`), `prompts.rs`, `ui/{mod,tasks,logger,sticker,table}.rs` |
| Integration tests | `crates/core/tests/http_smoke.rs`, `crates/core/tests/env_and_config_loading.rs` |

**Out of scope**: `lab/` (excluded from the workspace, experimental scratch), `target/`,
`Cargo.lock` (dependency pins are fixed by `10-tech-stack.md`, not reviewed here),
`.opencode/` (harness wiring).

Baseline facts (already verified): build passes, 19 tests pass, `cargo fmt --all --check`
clean, `cargo clippy --workspace --all-targets -D warnings` clean, `#![forbid(unsafe_code)]`
on the `rume` crate.

## Lenses

Five lenses, one report file each. Every lens writes to
`docs/reports/reviews/v01/<lens-name>.md`. Report format per lens: findings with
severity (C / M / m per `review-authoring` convention), the code under
finding, the break it catches, and a verdict. Lenses do not review their own
work (`50-agent-conduct.md`); each lens is executed by a fresh subagent.

### 1. Correctness — `correctness.md`

**Charter**: Does the code do what it claims? State machines, parse/validate
logic, error mapping, and route/health behavior must match their docs and
signatures.

**Code covered**:
- `crates/core/src/routing.rs` — `Matcher::{Number,Uuid,Slug}::validate`, `ValidatedParam<M>`
  extractor (pulls from `Path<BTreeMap<String,String>>`, validates, maps to
  `Rejection`), `RouteSpec::{get,post,put,patch,delete,where_param,r#where,r#as,build}`
  (registers into the global `ROUTE_REGISTRY` static and returns metadata),
  `RouteGroup<S>::{new,prefix,r#as,add,build}`, `health_routes<E,C>` (`/health/live`,
  `/health/ready` via `State<AppContext<E,C>>`), `registered_routes()`, `Rejection`
  → `IntoResponse` status mapping.
- `crates/core/src/app.rs` — `Lifecycle`/`Availability` atomic `u8` state machines
  (`Created=0 … Terminated=4`), `RumeBuilder::run()` (bind, `on_ready`/`on_shutdown`
  hooks, graceful shutdown, availability transitions
  `Live→Ready→RefusingTraffic→Unknown`), `ops_router<S>` (captures
  `Availability` directly, not from Axum state).
- `crates/core/src/error.rs` — `AppError::status_code()` mapping and `IntoResponse`
  bodies (`Validation` includes `details`; all others render `to_string()`).
- `crates/core/src/validation.rs` — `Validated<T>` extractor (`Json` extraction then
  `garde::Validate`; `BadRequest` 400 vs `Validation` 422 mapping).

**Checks**:
- State-machine transitions and the `match` default arms (any `u8` value not
  1–4 maps to `Created`/`Unknown` — is that intended?).
- Matcher edge cases: UUID segment lengths/hex digits, slug leading/trailing
  `-`/`_`, `--`/`__`, empty values, `value.parse::<u64>()` for `Number`.
- `Rejection` variants map to the documented status codes (400/400/422).
- `health_ready` returns 503 exactly when `is_healthy == false`; `health_live`
  returns 503 only when availability is `Broken`; `ops_router` ready uses
  `is_ready()` (`== Ready` only, not `Live`).
- `ROUTE_REGISTRY` lock poisoning is handled (`unwrap_or_default`) and route
  registration side-effect happens in `build()` even if the router is never
  mounted.
- Error bodies do not drop or corrupt detail fields; `Internal`/`BadRequest`
  render `self.to_string()` (leakage is the security lens's call, correctness
  verifies the mapping is as documented).

**Dependencies**: none (stage 1).

### 2. Security — `security.md`

**Charter**: Input validation, secret handling, information leakage. Anything
that reaches the network, the filesystem, logs, or the terminal from
untrusted or configured input.

**Code covered**:
- `crates/core/src/env.rs` — `resolve_value` `file:` prefix resolution (relative paths
  joined with `dir`, **no `..` traversal guard**), `file\:` escape, secrets
  held as plain `String` in `ResolvedEnv.values` (not `secrecy::SecretBox`),
  `redact()` heuristic (`SECRET`/`PASSWORD`/`TOKEN`/`_KEY`/`KEY`,
  `value_looks_sensitive` URL-with-`@` credentials), `resolve_env_name()`
  precedence (`RUME_ENV` > `RAILS_ENV` > `NODE_ENV` > `development`).
- `crates/core/src/error.rs` — `AppError::into_response` renders `self.to_string()`
  into the JSON `error` field for `Internal`/`BadRequest`/`NotFound`/`Unauthorized`/`Forbidden`
  (internal detail leakage to clients); `Validation` details echo field paths
  and garde messages.
- `crates/core/src/routing.rs` — `health_routes` `/health/ready` `debug_info` includes
  `pid` and `platform`; `Rejection` messages echo parameter names and values
  (`"{value} is not a valid number"`); `ValidatedParam` rejection includes
  param name.
- `crates/cli/src/commands/env/check.rs` — `env:check --all` prints process
  environment variables to the terminal (possible secret disclosure).
- Workspace `Cargo.toml`: `secrecy` is a pinned dependency; verify it is
  actually used for secret-typed config/env (doc example in `env.rs` shows
  `secrecy::SecretBox<String>`).

**Dependencies**: none (stage 1).

### 3. Architecture — `architecture.md`

**Charter**: Crate boundaries, explicitness, no magic — per `60-architecture.md`
(refusals: no DI container, no scanning, no hidden AOP, no mandatory ORM; Env/
Config split; core must not depend on heavy optional crates; CLI in `rume-cli`).

**Code covered**:
- `crates/core/src/lib.rs` — module graph, `#![forbid(unsafe_code)]`, re-exports of
  `axum`/`serde`/`serde_json`/`tokio`/`tracing` from core (heavy re-exports
  vs "core must not depend on heavy optional crates"), `cfg(feature =
  "schemars")` on `config::schema` (feature gating boundary).
- `crates/core/src/app.rs` — `AppContext<E, C>` carries env/config/paths/availability/lifecycle/health_checks;
  `Rume::builder(ctx)` is the single entry point (`builder()`,
  `builder_with_state()`, `builder_with_context()` removed); handlers access
  everything via `State<AppContext<E, C>>`; lifecycle hooks are visible `FnOnce`
  closures, not proxied.
- `crates/core/src/env.rs` vs `crates/core/src/config.rs` — Env/Config split: layered `.env`
  files + process env (`dotenvy` + figment `Serialized`) vs layered TOML under
  `config/` (`figment` `Toml` provider); both validated into Rust types at boot.
- `crates/core/src/prelude.rs` — re-export surface and discoverability.
- `rume-cli/` — depends on `rume`, no code generation by scanning; `codegen`
  regenerates barrel/registration files explicitly; `doctor` validates project
  setup.
- `lab/` — excluded from the workspace (experimental, allowed to break pre-v1).

**Dependencies**: none (stage 1).

### 4. DX/Usability — `dx-usability.md`

**Charter**: Is the API ergonomic and discoverable? Naming, entry points,
fluent builders, error messages, CLI UX. Depends on stage 1 for the stability
of the APIs being judged.

**Code covered**:
- `crates/core/src/app.rs` — `Rume::builder(ctx)` single entry point; `AppContext::new`
  6-argument constructor generic over `E, C`; `RumeBuilder` fluent setters
  (`route`/`router`/`bind`/`mode`/`environment`/`paths`/`on_ready`/`on_shutdown`);
  `ops_router` vs `routing::health_routes` — two health-route implementations
  with different names, state sources, and status shapes (post-rename confusion
  surface).
- `crates/core/src/routing.rs` — `RouteSpec` fluent API (`r#where`/`r#as` raw-identifier
  aliases, `where_param`), `RouteGroup`, `Matcher` enum, `ValidatedParam`
  deref-to-`str` + `into_inner`.
- `crates/core/src/prelude.rs` and `lib.rs` — what is re-exported and where users
  find it; doc examples use `#[ignore]` (not compiled as doctests).
- `crates/cli/src/cli.rs` + commands — `new`, `env check [--all]`, `config check`,
  `doctor`, `completions`, `codegen`, `generate` (removed `dev`/`test`,
  added `list`/`completions` per context); UX of inspection/output.
- `crates/core/src/error.rs` — rejection/AppError message wording for end users.

**Dependencies**: stage 1 (Correctness, Architecture, Security).

### 5. Testing — `testing.md`

**Charter**: Are tests sufficient, and do they cover the new code? Per
`testing.md`: pure logic → hermetic `#[cfg(test)]` in `src/`; DB/HTTP surface
→ `tests/` via the shared harness; no silent skips; `#[ignore]` only for
`todo_*` stubs / review-tracked known-broken; fails-before proof required.

**Code covered**:
- `crates/core/tests/http_smoke.rs` — one HTTP smoke test (bare `/health` router,
  not the real `health_routes`/`ops_router`).
- `crates/core/tests/env_and_config_loading.rs` — layered env files, `.env.local`
  skipping in test mode, `file:` resolution, merged config, typed env.
- Hermetic unit modules in `src/` — `app.rs` (env/availability/lifecycle),
- `env.rs` (redaction/secret detection), `validation.rs` (Validated extractor
  via `TestApp`), `error.rs` (status mapping).
- Untested surface: **all of `routing.rs`** (no `#[cfg(test)]` module:
  `RouteSpec`, matchers, `ValidatedParam`, `RouteGroup`, `HealthChecks::run`,
  `health_routes`, `Rejection`, `registered_routes`); `config.rs` (no tests);
- `testing.rs` (`TestApp`/`TestAppBuilder` harness itself, untested).
- `crates/core/src/env.rs` uses `unsafe { std::env::set_var/remove_var }` in tests
  with an `ENV_LOCK` Mutex — global-process-env mutation and test isolation;
- Global `ROUTE_REGISTRY` static — cross-test pollution via `RouteSpec::build`.

**Dependencies**: stage 1 (Correctness, Security) — testing lens judges
coverage against the gaps those lenses identify.

## Staged execution graph (≤ 3 stages)

```
Stage 1 (parallel, independent):  Correctness → correctness.md
                                  Security    → security.md
                                  Architecture → architecture.md
Stage 2 (depends on Stage 1):     DX/Usability → dx-usability.md
                                  Testing      → testing.md
Stage 3:                          none needed
```

- **Stage 1**: Correctness, Security, Architecture read the same codebase
  independently and produce their reports in parallel. No lens depends on
  another stage-1 lens.
- **Stage 2**: DX/Usability and Testing consume stage-1 reports (DX judges the
  APIs correctness/security pinned down; Testing judges coverage against
  correctness/security gaps). Stage 2 is the **last stage**, so its agents
  write the lens reports `dx-usability.md` and `testing.md`. Stage-1 agents
  write `correctness.md`, `security.md`, `architecture.md` at the close of
  stage 1.
- Every lens report path: `docs/reports/reviews/v01/<lens-name>.md`.

## Output paths

| Lens | Report file |
|---|---|
| Correctness | `docs/reports/reviews/v01/correctness.md` |
| Security | `docs/reports/reviews/v01/security.md` |
| Architecture | `docs/reports/reviews/v01/architecture.md` |
| DX/Usability | `docs/reports/reviews/v01/dx-usability.md` |
| Testing | `docs/reports/reviews/v01/testing.md` |

## Conventions

- Each report names the break each finding catches (production change that
  should fail the test or review), with expected values derived by hand, not
  by the code under review (`80-quality-bar.md`).
- Severity C / M / m; a finding cites the relevant `Why:` rationale comment
  where one exists (`70-code-rationale.md`).
- No agent reviews its own lens; one implementer at a time per lens
  (`50-agent-conduct.md`). Stage 1 subagents run in parallel; stage 2 begins
  only after stage 1 reports exist.
- Rulings made during execution are recorded in the run ledger and the code
  records why (`70-code-rationale.md`).