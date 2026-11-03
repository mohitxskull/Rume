# TODO — Rume Whole-Document Review (v04) Remediation Plan

Derived from the 10 lens reports in `docs/reports/reviews/v04/` and the cross-lens consistency pass in `docs/reports/reviews/v04/consistency.md`.
Ordered by execution priority `P0`–`P3` (what unblocks what), with shared root causes and tightly-coupled subsystems grouped for solving together.

---

## P0 — Critical Availability, Security & Secret Protection (fix immediately before any traffic exposure)

### Group A — Application Readiness & Availability Lifecycle (solve together)
> Root cause: `routing/health.rs` and `app/state.rs` fail to enforce a strict three-phase lifecycle (Live → Ready → RefusingTraffic), causing traffic to be routed prematurely during boot and admitted during shutdown.

- [x] **Routing C1 — Readiness probe `/health/ready` returns 200 OK during startup** (`routing-api-openapi.md` C1).
      `rume/src/routing/health.rs:335`: `state == AvailabilityState::Ready || state == AvailabilityState::Live` returns 200 OK before module and application startup hooks execute. Change check to strictly require `state == AvailabilityState::Ready`.
- [x] **App M5 — `Availability::set` allows unconstrained drain bypass** (`app-lifecycle.md` M5).
      `rume/src/app/state.rs:159–167`: `Availability::set` allows arbitrary external state mutation, permitting workers or handlers to overwrite `RefusingTraffic` with `Ready`. Enforce monotonic forward transitions (reject transition to `Ready` if current state is `RefusingTraffic` or `Terminating`).
- [x] **App m4 — `/health/live` returns 200 OK for `AvailabilityState::Unknown`** (`app-lifecycle.md` m4).
      `rume/src/routing/health.rs:320`: Unmapped atomic values return 200 OK. Return 503 Service Unavailable when state is `Unknown`.

### Group B — Secret Redaction & Token Security (solve together)
> Root cause: Diagnostic CLI commands and default middleware configurations lack fail-closed secret masking and token verification.

- [x] **CLI C1 — Multi-line secret leakage in `rume config check`** (`dx-cli.md` C1).
      `rume-cli/src/commands/config.rs:114–119`: Line-by-line splitting leaks multi-line RSA and PEM secret blocks on continuation lines. Apply `redact_value` to the entire string before line splitting.
- [x] **Security M1 — Insecure defaults in `CsrfConfig` allow unauthenticated double-submit tokens** (`security-protocols.md` M1).
      `rume/src/security/csrf.rs:56–65`: `secret: None` allows unauthenticated tokens without startup validation. Require explicit opt-in for unauthenticated tokens; fail closed at boot if HMAC secret is missing in production.
- [x] **Security M2 — Subdomain cookie-tossing DoS in `SessionScheme`** (`security-protocols.md` M2).
      `rume/src/auth/session.rs:67–76`: Aborts cookie extraction on the first matching cookie name, enabling denial-of-service via tossed parent-domain cookies. Iterate through all matching cookie headers/values until a valid session is found.
- [x] **CLI M1 — `rume config explain` outputs plaintext secrets without redaction** (`dx-cli.md` M1).
      `rume-cli/src/commands/config.rs:14–35`: Dumps configuration entries in plaintext without `--unredacted` gating. Apply `mask_secret` / `redact_value` to secret fields by default.

---

## P1 — Core Architecture, App Lifecycle & Coordination Integrity (fix before production release)

### Group C — Dynamic OpenAPI & Route Registration (solve together)
> Root cause: Eager compilation of OpenAPI specs during builder construction discards all later-registered domain module routes and subrouters.

- [x] **App M1 — `RumeBuilder::openapi()` freezes route registry eagerly, omitting module routes** (`app-lifecycle.md` M1).
      `rume/src/app/builder.rs:739–746, 1088–1091`: `OpenApiSpec::from_routes` is generated eagerly when `.openapi(...)` is called, discarding all module endpoints. Defer OpenAPI generation to preflight completion via lazy evaluation on the final merged registry.
- [x] **Routing M1 — `Router::openapi()` captures static snapshot at registration time** (`routing-api-openapi.md` M1).
      `rume/src/routing/router.rs:361–369`: Static routes snapshot misses subsequently mounted subrouters. Wire OpenAPI endpoint to query the shared `Arc<RouteRegistry>` dynamically.
- [x] **Routing M2 — OpenAPI parameter schema regex for `Matcher::Slug` contradicts runtime** (`routing-api-openapi.md` M2).
      `rume/src/openapi/spec.rs:169–173`: Pattern `^[a-z0-9]+(?:-[a-z0-9]+)*$` contradicts `matchers.rs:66–85` which accepts underscores, uppercase letters, and lengths up to 128. Align regex pattern with runtime validator.

### Group D — Module Preflight & Boundary Verification (solve together)
> Root cause: Route collision preflight and architecture rule verification do not check transitive reachability or overlapping wildcard methods.

- [x] **App M4 — Module route collision preflight ignores `ANY` matcher overlap, triggering Axum panics** (`app-lifecycle.md` M4).
      `rume/src/app/builder.rs:964–984`: Checks method equality without handling `MethodFilter::all()` / `ANY` overlaps, causing Axum's `Router::merge` to panic at runtime. Check bitwise intersection of method filters on identical paths; fail closed with `BuildError::DuplicateRoute`.
- [x] **Architecture M1 — `ArchitectureRules::verify` ignores undeclared dependencies and cycles** (`architecture-module-boundaries.md` M1).
      `rume/src/module/architecture.rs:84–123`: Traverses only explicitly declared dependencies, allowing transitive forbidden calls to bypass CI. Traverse transitive dependency graph and detect cycles during `verify()`.

### Group E — Redis & Multi-Tenant Coordination Isolation (solve together)
> Root cause: Unnamespaced Redis keys, destructive full-database flushes, and flawed tag expiration allow cross-tenant and cross-subsystem state corruption.

- [ ] **Coordination M1 — `RateLimitService` reads a tenant extension type never inserted** (`coordination-multi-tenant.md` M1).
      `rume/src/resilience/rate_limiter.rs:64–72`: Reads `TenantContext` from extensions where it is never inserted, rendering tenant rate limiting inert. Extract `TenantContext` using standard Axum extractor.
- [ ] **Coordination M2 — `RedisCacheBackend` default prefix `rume:` collides with other subsystems** (`coordination-multi-tenant.md` M2).
      `rume/src/cache/redis.rs:88–92`: Collides with session, lock, and idempotency namespaces. Isolate default cache prefix to `rume:cache:`.
- [ ] **Coordination M3 — `Cache::clear()` executes destructive `FLUSHDB`** (`coordination-multi-tenant.md` M3).
      `rume/src/cache/redis.rs:97–104`: Executes `FLUSHDB`, wiping all keys on the shared logical database. Scan and delete keys matching the configured cache prefix only.
- [ ] **Coordination M4 — Tag-set `PEXPIRE` unconditionally shortens tag lifetime** (`coordination-multi-tenant.md` M4).
      `rume/src/cache/redis.rs:110–125`: Shortens tag set expiry, causing tag invalidations to prematurely no-op. Use `PEXPIRE ... GT` or refresh to maximum constituent key TTL.
- [ ] **Coordination M5 — `TenantScopedCache::invalidate_tag` releases quota for oldest keys** (`coordination-multi-tenant.md` M5).
      `rume/src/tenant/cache.rs:139–158`: Quota release frees permits for the oldest tracked keys rather than the keys actually deleted. Release quota for the exact keys deleted by tag invalidation.
- [ ] **Coordination M8 — `remember_with_tags` stampede waiters can block forever** (`coordination-multi-tenant.md` M8).
      `rume/src/cache/typed.rs:208–225`: Waiter creates `Notify` listener after checking in-flight status, missing notifications if probe completes quickly. Register notification waiter before checking in-flight status.

### Group F — SAML Hardening & Protocol Conformance (solve together)
> Root cause: XMLDSig enveloped root document signatures and missing expiration conditions are rejected or improperly accepted.

- [ ] **Security M3 — SAML parser rejects enveloped document signatures (`URI=""`) and fails open on missing expiration** (`security-protocols.md` M3).
      `rume/src/enterprise/saml.rs:147–185`: Rejects RFC 3275 `URI=""` document signatures, and `validate()` accepts assertions omitting `NotOnOrAfter`. Support `URI=""` for root response signatures; require `NotOnOrAfter` to be present or fail closed in `validate()`.

---

## P2 — Major Robustness, CLI Scaffolding & Database Hygiene (fix before multi-node / public rollout)

### Group G — Audit Logging & Persistence Parity (solve together)
> Root cause: `SqlAuditLogger` relies on string concatenation and hardcodes PostgreSQL DDL across all database backends.

- [ ] **Security M4 — `SqlAuditLogger` unparameterized queries & hardcoded PostgreSQL DDL** (`security-protocols.md` M4).
      `rume/src/security/audit.rs:76–112`: Uses unescaped string formatting and executes PostgreSQL-specific schema on MySQL and SQLite. Rewrite using parameterized queries and dialect-specific DDL.
- [ ] **Pool M1 — `DatabasePool::close_and_drain` does not close backend pools** (`pool-lifecycle-persistence.md` M1).
      `rume/src/persistence/pool.rs:120–145`: Closes pool handle without terminating underlying database backend driver connections. Invoke `self.backend.close().await` inside `close_and_drain`.
- [ ] **Outbox M1 — `outbox_insert.sql` lacks conflict handling** (`outbox-data-integrity.md` M1).
      `rume/src/persistence/sql/{postgres,mysql,sqlite}/outbox_insert.sql`: Re-inserting existing message ID fails on primary key. Add `ON CONFLICT (id) DO NOTHING` (or `INSERT IGNORE`).
- [ ] **Outbox M2 — Saga rollback ignores compensation errors** (`outbox-data-integrity.md` M2).
      `rume/src/saga/mod.rs:85–102`: Swallows step compensation errors during saga abort. Track and return `CompensationFailed` error details.
- [ ] **Coordination M6 — PostgreSQL leader `try_acquire` candidate ID divergence** (`coordination-multi-tenant.md` M6).
      `rume/src/coordination/postgres.rs:167–182`: `OR rume_leaders.candidate_id = $2` diverges from Redis term semantics and self-fences. Align incumbent re-acquisition with Redis term semantics.
- [ ] **Coordination M7 — Tenant registration admits silent collisions** (`coordination-multi-tenant.md` M7).
      `rume/src/tenant/resolver.rs:188–205`: Slug/ID shadowing logs warning and continues. Return `TenantError::Conflict` on any duplicate identifier or subdomain registration.

### Group H — CLI Component Generation & Diagnostics (solve together)
> Root cause: CLI generators lack input identifier validation, atomic rollback, and non-TTY error handling.

- [ ] **CLI M2 — `rume doctor` port availability check reports false-positive** (`dx-cli.md` M2).
      `rume-cli/src/commands/doctor.rs:45–58`: `starts_with("port ")` matches error `"port 3000 is in use"`, reporting `ok: true`. Check `TcpListener::bind` success explicitly.
- [ ] **CLI M3 — Component generators accept non-identifier names** (`dx-cli.md` M3).
      `rume-cli/src/commands/new.rs:88–102`: Allows digit-prefixed and non-identifier names (`2fa`, `404`), generating invalid Rust syntax. Validate names against Rust identifier grammar (`[a-zA-Z_][a-zA-Z0-9_]*`).
- [ ] **CLI M4 — Non-transactional component generation leaves orphans** (`dx-cli.md` M4).
      `rume-cli/src/commands/new.rs:120–145`: Route marker insertion failure leaves orphaned handler files on disk. Implement atomic generation with cleanup rollback on write failure.
- [ ] **CLI M5 — Interactive prompts swallow Ctrl+C in non-TTY environments** (`dx-cli.md` M5).
      `rume-cli/src/prompts.rs:25–48`: Hangs or swallows cancellation in headless CI/CD. Check `io::stdin().is_terminal()`; fail closed with descriptive error in non-interactive environments.

### Group I — Application Lifecycle Shutdown & Health Probe Stampede (solve together)
> Root cause: Teardown hook ordering diverges between test harness and server, and probe cache expiration allows thundering herd races.

- [ ] **App M2 — `build_test_app()` inverts shutdown hook ordering** (`app-lifecycle.md` M2).
      `rume/src/app/builder.rs:602–613`: Runs module shutdown before application shutdown hooks (inverting `run()` order). Align shutdown hook sequence to run application hooks first, followed by module hooks.
- [ ] **App M3 — Shutdown signal unmonitored during startup hooks** (`app-lifecycle.md` M3).
      `rume/src/app/builder.rs:1157–1168`: Boot process hangs indefinitely if a module startup hook blocks on external dependency. Wrap startup hook execution with `tokio::select!` monitoring shutdown signal.
- [ ] **Routing M3 — Health check probe race on cache expiry causes thundering herd** (`routing-api-openapi.md` M3).
      `rume/src/routing/health.rs:186–210`: Concurrent requests during TTL expiry trigger duplicate probe executions. Protect probe execution with singleflight stampede suppression.
- [ ] **Jobs M1 — Redis job queue computes lease expiry from host clock** (`jobs-worker-concurrency.md` M1).
      `rume/src/jobs/redis.rs:72–95`: Uses `SystemTime::now()` for job lease expiry scores. Use Redis server-side `TIME` command to evaluate lease deadlines.
- [ ] **Jobs M2 — Redis job worker cancellation token ignored in handler** (`jobs-worker-concurrency.md` M2).
      `rume/src/jobs/worker.rs:114–135`: Lost lease heartbeat does not cancel active running handler. Propagate cancellation signal to `JobContext` during active execution.

### Group J — Crate Boundaries & Dependency Hygiene (solve together)
> Root cause: Enterprise XML parser is unconditionally pulled into core library, and crate root re-exports expose internal types.

- [ ] **Architecture M2 — Core crate unconditionally depends on `quick-xml`** (`architecture-module-boundaries.md` M2).
      `rume/Cargo.toml:46`: `quick-xml` pulled into minimal builds without enterprise features. Gate `quick-xml` behind an optional `saml` / `enterprise` feature.
- [ ] **Architecture M3 — Crate root `lib.rs` violates re-export discipline** (`architecture-module-boundaries.md` M3).
      `rume/src/lib.rs:35–123`: Flattens 70+ domain types into `rume::*`. Prune root re-exports to core primitives (`Rume`, `AppError`, `Event`); keep subsystem types in their respective modules.

---

## P3 — Test Suite Determinism, Harness Cleanup & Minor Polish (schedule as non-blocking polish)

### Group K — Test Determinism & Harness Upgrades (solve together)
> Root cause: Hardcoded wall-clock sleeps and orphaned harness definitions create test flakes and compiler warnings.

- [ ] **Testing M1 — 33 wall-clock `sleep()` calls in test suites** (`testing-feature-matrix.md` M1).
      `rume/tests/`: Adds >12s idle latency and causes timing flakes on loaded runners. Replace wall-clock sleeps with `tokio::sync::Notify`, virtual time, or polling conditions.
- [ ] **Testing M2 — Orphaned `RumeTestHarness` with zero adoption & compiler warnings** (`testing-feature-matrix.md` M2).
      `rume/src/testing/harness.rs:12–65`: Unused in tests and emits warnings under non-sqlite features. Resolve feature gates and integrate into integration suites.
- [ ] **Testing M3 — Tautological third-party mock test in enterprise integration** (`testing-feature-matrix.md` M3).
      `rume/tests/integration/enterprise.rs:300–315`: Asserts `wiremock` against `ureq` with zero Rume code exercised. Test real Rume IdP metadata resolution client.
- [ ] **Testing M4 — Missing negative-path coverage for events, sagas, and worker fences** (`testing-feature-matrix.md` M4).
      `rume/tests/`: Untested error paths in new pipeline code. Add unit and integration assertions for listener panics, saga step compensation errors, and worker lease fences.

### Group L — Minor Polish & Diagnostics (solve together)
- [ ] **App m1 — Stale code rationale comment on `RumeBuilder::route`** (`app-lifecycle.md` m1).
      `rume/src/app/builder.rs:218`: Comment claims route registration is delayed when it is now eager. Update comment to match active behavior.
- [ ] **App m2 — `ObservabilityLayer` checks `MatchedPath` outside router** (`app-lifecycle.md` m2).
      `rume/src/app/observability.rs:45`: Span `route` field is permanently empty. Capture matched path inside router middleware.
- [ ] **App m3 — `RumeBuilder::run` rejects `localhost` hostnames** (`app-lifecycle.md` m3).
      `rume/src/app/builder.rs:1042`: Parses addresses strictly as `SocketAddr`. Support `ToSocketAddrs` for hostname resolution.
- [ ] **App m5 — `build_test_app()` ignores `ApplicationMode::Inspect`** (`app-lifecycle.md` m5).
      `rume/src/app/builder.rs:592`: Inspect mode is not respected in test app. Align inspect handling with `run()`.
- [ ] **App m6 — `TestApp` drop blocks Tokio runtime thread via OS thread join** (`app-lifecycle.md` m6).
      `rume/src/testing/app.rs:88`: Blocking thread join inside async context. Use async task handle abort and drain.
- [ ] **Architecture m1 — `prelude.rs` exports test harnesses into production** (`architecture-module-boundaries.md` m1).
      `rume/src/prelude.rs:8, 13`: Exports `TestApp` and `ModuleHarness` in production prelude. Move test utilities behind `#[cfg(feature = "testing")]`.
- [ ] **Architecture m2 — `RumeBuilder::state()` rejects state transitions due to type pinning** (`architecture-module-boundaries.md` m2).
      `rume/src/app/builder.rs:208`: Prevents state re-assignment. Support type-transforming state transition or clarify doc comment.
- [ ] **Architecture m3 — `lib.rs` re-exports deprecated `ops_router`** (`architecture-module-boundaries.md` m3).
      `rume/src/lib.rs:35–36`: Re-exports deprecated helper at crate root. Remove deprecated export.
- [ ] **Architecture m4 — Missing stability tiers in public API docs** (`architecture-module-boundaries.md` m4).
      `rume/src/lib.rs:1–7`: Add module stability badges (`Stable`, `Evolving`, `Experimental`).
- [ ] **Coordination m1 — Host wall-clock drives Redis rate-limit/quota windows** (`coordination-multi-tenant.md` m1).
      `rume/src/resilience/redis.rs:84`: Use Redis server time for sliding window calculation.
- [ ] **Coordination m2 — Subdomain resolver case sensitivity on IPv6** (`coordination-multi-tenant.md` m2).
      `rume/src/tenant/resolver.rs:114`: Strip brackets and normalize IPv6 hosts.
- [ ] **DX m1 — Inconsistent CLI argument flags for JSON output** (`dx-cli.md` m1).
      `rume-cli/src/commands/list_routes.rs:24`: Support `--format json` consistently across all CLI subcommands.
- [ ] **DX m2 — Dead UI code with crate-level `#![allow(dead_code)]`** (`dx-cli.md` m2).
      `rume-cli/src/ui/mod.rs:1–15`: Prune unused UI primitives and remove crate-level dead code attribute.
- [ ] **DX m3 — Generated smoke test template forces `unsafe` in Rust 2024** (`dx-cli.md` m3).
      `rume-cli/src/templates/tests_smoke_rs.tpl:12`: Replace `std::env::set_var` with safe environment loading.
- [ ] **DX m4 — `rume codegen` lacks dry-run support** (`dx-cli.md` m4).
      `rume-cli/src/commands/codegen.rs:34`: Add `--dry-run` flag showing planned file modifications.
- [ ] **Jobs m1 — `AssertUnwindSafe` around construct instead of future** (`jobs-worker-concurrency.md` m1).
      `rume/src/jobs/worker.rs:188`: Wrap the executed future with `catch_unwind` rather than construction closure.
- [ ] **Jobs m2 — DLQ routing on heartbeat loss** (`jobs-worker-concurrency.md` m2).
      `rume/src/jobs/worker.rs:215`: Move lost-heartbeat jobs to DLQ rather than leaving them in limbo.
- [ ] **Outbox m1 — `EventBus` downcasting safety** (`outbox-data-integrity.md` m1).
      `rume/src/events/mod.rs:120`: Document and assert type-safe event handler downcasting.
- [ ] **Outbox m2 — In-flight lease expiration SQL edge case** (`outbox-data-integrity.md` m2).
      `rume/src/persistence/sql/postgres/outbox_claim.sql`: Ensure exact millisecond bounds matching database clock.
- [ ] **Pool m1 — Pool telemetry dead atomics** (`pool-lifecycle-persistence.md` m1).
      `rume/src/persistence/pool.rs:88`: Wire `total_connections` atomic to actual driver pool sizing metrics.
- [ ] **Pool m2 — `auto_migrate` is inert** (`pool-lifecycle-persistence.md` m2).
      `rume/src/persistence/pool.rs:160`: Wire `apply_auto_migrations` to pool initialization or remove inert configuration.
- [ ] **Pool m3 — Four drifting copies of `rume_migrations` DDL** (`pool-lifecycle-persistence.md` m3).
      `rume/src/persistence/migration.rs:45`: Consolidate to single source of truth for migration ledger schema.
- [ ] **Pool m4 — Migration ledger `applied_at` uses host clock** (`pool-lifecycle-persistence.md` m4).
      `rume/src/persistence/migration.rs:112`: Evaluate migration `applied_at` via database clock.
- [ ] **Pool m5 — `DatabasePool::close()` is fire-and-forget** (`pool-lifecycle-persistence.md` m5).
      `rume/src/persistence/pool.rs:135`: Add async close awaiting physical connection teardown.
- [ ] **Pool m6 — Dead scaffolding `MigrationFn`** (`pool-lifecycle-persistence.md` m6).
      `rume/src/persistence/migration.rs:32`: Prune unused `MigrationFn` definition.
- [ ] **Routing m1 — `/health/ready` collapses Warning into error in body while returning 503** (`routing-api-openapi.md` m1).
      `rume/src/routing/health.rs:350`: Preserve `"status": "warning"` in response payload when reporting 503.
- [ ] **Routing m2 — `MetricsLayer` hardcodes bypass for exact path `"/metrics"`** (`routing-api-openapi.md` m2).
      `rume/src/metrics/layer.rs:67`: Dynamically match configured metrics endpoint path rather than hardcoded string.
- [ ] **Routing m3 — Route parameter matcher declaration does not validate against path template** (`routing-api-openapi.md` m3).
      `rume/src/routing/spec.rs:101`: Assert declared parameter names exist within path template during route registration.
- [ ] **Routing m4 — OpenAPI 3.1 generator lacks Request Body and Security Schemes** (`routing-api-openapi.md` m4).
      `rume/src/openapi/spec.rs:83`: Populate request body schemas and auth security schemes in generated document.
- [ ] **Security m1 — `Forwarded` header substring matching and unchecked proxy trust** (`security-protocols.md` m1).
      `rume/src/security/headers.rs:88`: Require trusted proxy CIDR configuration before honoring `X-Forwarded-*`.
- [x] **Security m2 — `CsrfService` reads only first Cookie header** (`security-protocols.md` m2).
      `rume/src/security/csrf.rs:120`: Parse split cookie headers across all incoming `Cookie` header lines.
- [ ] **Security m3 — Non-atomic session ID rotation in `RedisSessionStore`** (`security-protocols.md` m3).
      `rume/src/auth/session.rs:145`: Execute session ID rotation in single atomic Lua script.
- [ ] **Security m4 — Dead error variant `SamlError::MissingSignatureVerifier`** (`security-protocols.md` m4).
      `rume/src/enterprise/saml.rs:30`: Clean up unused error variant and enforce mandatory verifier checks.
- [ ] **Security m5 — SCIM 2.0 casing and attribute type deviations** (`security-protocols.md` m5).
      `rume/src/enterprise/scim.rs:48`: Align `Resources` list key casing and complex roles with RFC 7643/7644.
- [ ] **Testing m1 — Unused import compiler warnings in `pipeline.rs` under non-sqlite builds** (`testing-feature-matrix.md` m1).
      `rume/tests/containers/pipeline.rs:25`: Clean up feature-gated imports to prevent build warnings.
- [ ] **Testing m2 — Redundant assertions duplicating `insta` snapshots** (`testing-feature-matrix.md` m2).
      `rume/tests/integration/openapi.rs:45`: Prune redundant manual field assertions that duplicate snapshot coverage.
- [ ] **Testing m3 — Inconsistent `#![forbid(unsafe_code)]` vs `#![deny(unsafe_code)]`** (`testing-feature-matrix.md` m3).
      `rume/tests/`: Align test harness crate-level lint attributes.

---

## Counts

| Priority Tier | Critical (C) | Major (M) | Minor (m) | Total |
|---|---|---|---|---|
| **P0 — Critical Availability, Security & Secrets** | 2 | 2 | 1 | **5** |
| **P1 — Core Architecture, Lifecycle & Coordination** | 0 | 9 | 0 | **9** |
| **P2 — Major Robustness, CLI & Persistence** | 0 | 15 | 0 | **15** |
| **P3 — Testing Determinism & Polish** | 0 | 4 | 28 | **32** |
| **Total** | **2** | **30** | **29** | **61** |
