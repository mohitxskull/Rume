# Rume v02 Review — todo.md

Derived from the 6 lens reports in `docs/reports/reviews/v02/`:
- `correctness.md` (C1, M1–M3, m1–m2)
- `security.md` (C1–C2, M1–M4, m1–m3)
- `architecture.md` (M1–M3, m1–m5)
- `enterprise-reliability.md` (C1–C2, M1–M6, m1–m4)
- `dx-tooling.md` (M1–M5, m1–m8)
- `testing.md` (M1–M4, m1–m3)

Ordered by priority tier (`P0` critical unblockers → `P3` minor hardening), with findings sharing a root cause or subsystem grouped for solving together. IDs are lens-qualified per canonical review-authoring standard. Duplicate findings surfaced across multiple lenses appear once citing both lenses.

---

## P0 — Critical (Fix first: live data leakage, memory exhaustion, or shutdown hangs)

> Fix before any production traffic or multi-tenant deployment.

### Group A — Multi-Tenant Boundary & Protocol Integrity (solve together)
> Root cause: Delimiter collisions and unanchored suffix matching allow cross-tenant spoofing and cache contamination.

- [x] **Security C2 — Multi-tenant cache key delimiter collision permits cross-tenant cache poisoning** (`security.md` C2).
      `crates/core/src/tenant/cache.rs:23–29`: Scoped keys use naive format `tenant:{tenant_id}:{key}`; tenant ID `"a:b"` with key `"c"` produces `tenant:a:b:c`, colliding with tenant `"a"` and key `"b:c"`. Disallow `:` in tenant IDs and keys or length-prefix/escape delimiters.
- [x] **Security M1 — Subdomain tenant resolver permits suffix-matching domain spoofing** (`security.md` M1).
      `crates/core/src/tenant/resolver.rs:126–140`: `host_no_port.strip_suffix(&self.base_domain)` permits `evilapp.platform.io` to match `app.platform.io`. Require strict `.` separator (`.{base_domain}`) when matching subdomains.
- [x] **Security M2 — `HeaderTenantResolver` shares a single map for IDs and slugs** (`security.md` M2).
      `crates/core/src/tenant/resolver.rs:47–53`: Registering both ID and slug into one `BTreeMap` allows an attacker tenant whose slug matches another tenant's ID to hijack requests. Separate ID and slug lookups into distinct maps or qualify keys (`id:`, `slug:`).
- [x] **Security C1 — SAML 2.0 assertion parser hardcodes expiration timestamps and lacks signature verification** (`security.md` C1).
      `crates/core/src/enterprise/saml.rs:149–155`: `SamlParser::parse` hardcodes `not_before: None` and `not_on_or_after: None`, leaving assertions perpetually valid regardless of XML conditions. Parse `NotBefore` and `NotOnOrAfter` from `<saml2:Conditions>` and verify XML digital signatures.

### Group B — Operational Outbox & Metrics Integrity (solve together)
> Root cause: Unbounded in-memory collections and missing distributed concurrency controls.

- [x] **Enterprise-Reliability C1 — Prometheus metrics recording stores unbounded raw float samples** (`enterprise-reliability.md` C1).
      `crates/core/src/metrics/registry.rs:81–89`: Latencies are stored in an unbounded heap `Vec<f64>`, causing process OOM crashes and O(N) scrape timeouts. Replace raw sample buffer with fixed histogram buckets or ring buffers.
- [x] **Enterprise-Reliability C2 — Concurrent `OutboxRelay` instances lack claiming locks in HA deployments** (`enterprise-reliability.md` C2).
      `crates/core/src/outbox/relay.rs:90–112`: Multiple replicas simultaneously call `fetch_pending` and process identical outbox messages without row locking or `InFlight` status transitions. Implement distributed lock guards or atomic claiming states (`InFlight`).

### Group C — Worker Panic Safety & Concurrency Bounds (solve together)
> Root cause: Missing task tracking guards and task spawn concurrency controls.

- [x] **Correctness C1 — Panic during job execution permanently leaks `in_flight_count` and hangs `drain()`** (`correctness.md` C1).
      `crates/core/src/jobs/worker.rs:133–141`: `in_flight.fetch_sub` is called only at the end of the spawned task; an unhandled panic bypasses decrement, causing `drain()` to hang indefinitely during shutdown. Use an RAII drop guard for `in_flight_count` decrement.
- [x] **Correctness M2 — `Job::timeout()` is never enforced during job execution** (`correctness.md` M2).
      `crates/core/src/jobs/worker.rs:154–173`: Worker executes `handler.execute(ctx)` without wrapping in `tokio::time::timeout`, leaving `JobError::Timeout` as dead code and allowing hung handlers to block worker drain. Wrap execution in `tokio::time::timeout`.
- [x] **Enterprise-Reliability M1 — `JobWorker` lacks shed-load concurrency bounds** (`enterprise-reliability.md` M1).
      `crates/core/src/jobs/worker.rs:120–135`: Queue poll loop spawns unbounded Tokio tasks for every fetched job without a semaphore or concurrency permit limit. Introduce a configurable concurrency limiter (`tokio::sync::Semaphore`).

---

## P1 — Major (Fix before any production release)

> Fix before public or production release milestone.

### Group D — Distributed Coordination & Recovery Wiring (solve together)
> Root cause: Disconnected background recovery routines and unversioned leadership leases.

- [x] **Enterprise-Reliability M2 — Distributed worker heartbeats and abandoned job recovery are disconnected dead code** (`enterprise-reliability.md` M2).
      `crates/core/src/jobs/reliability.rs:115–145`: `HeartbeatRegistry` and dead worker job reclamation exist in isolation and are never invoked by `JobWorker`. Wire periodic heartbeats and recovery into the worker loop.
- [x] **Enterprise-Reliability M6 — `LeaderElector` lacks monotonic fencing tokens or epoch tracking** (`enterprise-reliability.md` M6).
      `crates/core/src/coordination/leader.rs:65–95`: Leadership election returns a boolean without lease fencing tokens or epoch counters, risking split-brain execution during network partitions. Add monotonic lease terms/fencing tokens to leader leases.

### Group E — Resilience & Error Mapping Robustness (solve together)
> Root cause: Missing transport error observation and uncollected state entries.

- [x] **Correctness M1 — `CircuitBreakerService` skips failure recording on downstream Tower / network `Err` responses** (`correctness.md` M1).
      `crates/core/src/resilience/circuit_breaker.rs:189–209`: `match inner.call(req).await` matches `Err(e)` and immediately returns without calling `breaker.record_failure()`, rendering the breaker blind to network drops. Record failures on `Err(e)` prior to returning.
- [x] **Enterprise-Reliability M3 — `RateLimiter` permanently leaks memory for inactive clients** (`enterprise-reliability.md` M3).
      `crates/core/src/resilience/rate_limiter.rs:40–75`: Client entries in `BTreeMap` are never purged, leaking memory monotonically under randomized client IPs. Implement background TTL cleanup or LRU eviction.
- [x] **Correctness m2 — `CircuitBreaker` permits unbounded concurrency during `HalfOpen` recovery probe** (`correctness.md` m2).
      `crates/core/src/resilience/circuit_breaker.rs:96–101`: `CircuitState::HalfOpen` allows all concurrent requests through instead of permitting a single trial probe. Restrict `HalfOpen` to a single trial permit.
- [x] **Security m1 — `AuthError::ProviderError` maps to HTTP 400 Bad Request instead of HTTP 500** (`security.md` m1).
      `crates/core/src/auth/error.rs:25–35`: Downstream identity provider internal failures are mapped to `StatusCode::BAD_REQUEST`, falsely blaming the client. Map `ProviderError` to `StatusCode::INTERNAL_SERVER_ERROR`.
- [x] **Security m2 — Stale session cookie in `SessionScheme` immediately aborts authentication chain** (`security.md` m2).
      `crates/core/src/auth/scheme.rs:175–195`: If an expired or invalid session cookie is presented alongside a valid API key or Bearer token, `SessionScheme` returns an error, halting multi-scheme fallback. Allow fallback to subsequent schemes on invalid session cookies.

### Group F — Cache Invalidation, Concurrency & Eviction (solve together)
> Root cause: Missing memory eviction bounds and stampede locks.

- [x] **Enterprise-Reliability M4 — `MemoryCacheBackend` retains expired entries indefinitely without TTL eviction** (`enterprise-reliability.md` M4).
      `crates/core/src/cache/backend.rs:30–65`: Entries whose `expires_at < now` are only ignored on read, never removed, leaking memory. Add periodic passive eviction or bounded capacity limits.
- [x] **Enterprise-Reliability M5 — `Cache::remember` lacks stampede (thundering herd) protection on cache miss** (`enterprise-reliability.md` M5).
      `crates/core/src/cache/typed.rs:75–95`: Multiple concurrent requests for an expired key all execute the fallback factory in parallel. Add singleflight deduplication on cache misses.
- [x] **Enterprise-Reliability m4 — `TenantScopedCache` and `Cache` omit tag prefix namespacing** (`enterprise-reliability.md` m4).
      `crates/core/src/tenant/cache.rs:20–40`: Cross-tenant invalidation tags can collide when user-defined tags overlap. Namespace all invalidation tags under `tenant:{id}:tag:{name}`.

### Group G — Architecture Enforcement & Module Lifecycle (solve together)
> Root cause: Shallow graph checks and panicking route merges.

- [x] **Architecture M2 / Testing M2 — `ArchitectureRules::verify` only inspects direct 1-hop dependencies** (`architecture.md` M2, `testing.md` M2).
      `crates/core/src/module/architecture.rs:65–85`: Rules check `declared.contains(&dep)` for immediate dependencies only, missing transitive forbidden cross-domain calls. Compute transitive closure when verifying module boundaries.
- [x] **Architecture M3 — Route merge collisions trigger unhandled runtime panics instead of preflight errors** (`architecture.md` M3).
      `crates/core/src/module/lifecycle.rs:70–85`: Axum's `.merge()` panics on duplicate route paths at runtime. Pre-validate route path uniqueness across modules in `RumeBuilder::build()`.
- [x] **Testing M4 — `TestApp` bypasses module lifecycle hooks** (`testing.md` M4).
      `crates/core/src/testing.rs:15–35`: `TestApp::new(router)` takes a pre-built router and never triggers `module.on_start()` or `module.on_shutdown()`. Add `TestApp::from_builder(builder)` to run the module lifecycle in memory.

### Group H — Security & Persistence Hardening (solve together)

- [x] **Security M3 — `ApiKeyScheme` stores plaintext API keys in memory with early-exit linear scan** (`security.md` M3).
      `crates/core/src/auth/scheme.rs:105–125`: Keys are stored as plaintext and checked with `==` in a linear loop, creating timing leaks. Store hashed keys (`sha256`) and verify using constant-time `safe_equal`.
- [x] **Security M4 — Double-submit CSRF layer lacks token issuance and is vulnerable to subdomain tossing** (`security.md` M4).
      `crates/core/src/security/csrf.rs:158–180`: Layer verifies cookies but provides no endpoint or helper to issue cryptographically signed tokens. Add CSRF token generator and optional `SameSite=Strict` cookie injector.
- [x] **Security m3 — `SecurityHeadersLayer` injects HSTS header unconditionally over plain HTTP** (`security.md` m3).
      `crates/core/src/security/headers.rs:155–165`: Strict-Transport-Security is injected even over plain HTTP connections, violating RFC 6797. Gate HSTS injection behind TLS/HTTPS connection state or `x-forwarded-proto: https`.
- [x] **Correctness M3 — `Migrator::rollback_last` silently unmarks migrations when `down_sql` is `None`** (`correctness.md` M3).
      `crates/core/src/persistence/migrator.rs:132–145`: If a migration has no `down_sql`, it is deleted from the executed tracker without running any rollback. Reject irreversible rollback attempts with `PersistenceError::IrreversibleMigration`.

---

## P2 — Major Robustness & Developer Experience (Fix before wider adoption)

### Group I — Public API Ergonomics & Prelude Hygiene (solve together)

- [x] **Architecture M1 — Over-broad `prelude.rs` leaks niche enterprise schemas and string helpers** (`architecture.md` M1).
      `crates/core/src/prelude.rs:14–25`: Prelude re-exports `ScimUser`, `SamlParser`, `DistributedLock`, `slug`, `escape_html`, cluttering autocomplete. Restrict prelude to everyday application staples; place protocols in `rume::enterprise::*`.
- [x] **DX M1 — OpenAPI 3.1 generator discards route parameter matchers and uses substring heuristics** (`dx-tooling.md` M1).
      `crates/core/src/openapi/generator.rs:85–115`: Schema types are guessed from substring patterns (`param.contains("uuid")`) instead of inspecting `RouteSpec.matchers`. Propagate declared `Matcher` types into OpenAPI parameter schemas.
- [x] **DX M2 — `AppContext::builder` fails to infer state generic and requires `S: Default`** (`dx-tooling.md` M2).
      `crates/core/src/app/context.rs:50–80`: Builder forces users to call `AppContext::new` with 6 positional arguments. Provide `AppContext::builder()` with sensible default state or `new_with_defaults()`.
- [x] **DX M3 — Configuration provenance reporting is entirely absent** (`dx-tooling.md` M3).
      `crates/core/src/config/loader.rs:30–60`: Config loader cannot report which file or env var supplied a given setting. Expose Figment provider provenance or metadata inspector.
- [x] **DX M4 — Environment interpolation error formatting hides variable name via escaped braces** (`dx-tooling.md` M4).
      `crates/core/src/env/interpolation.rs:45–60`: Error string displays literal `${{variable}}` instead of actual unresolved key. Fix format string to show the missing variable name.
- [x] **DX m4 — `RouteGroup` lacks fluent `.layer()` middleware mounting method** (`dx-tooling.md` m4).
      `crates/core/src/routing/group.rs:60–80`: `RouteGroup` allows adding routes but cannot mount group-wide Tower middleware. Add `.layer(L)` to `RouteGroup`.
- [x] **DX m5 / Architecture m5 — `RumeBuilder` lacks fluent `.openapi()`, `.metrics()`, and state transition** (`dx-tooling.md` m5, `architecture.md` m5).
      `crates/core/src/app/builder.rs:75–130`: OpenAPI and Prometheus endpoints can only be mounted on inner `Router`, not `RumeBuilder`. Add `.openapi()` and `.metrics()` directly to `RumeBuilder`.

### Group J — Scaffolding, CLI & Template Alignment (solve together)

- [x] **DX M5 — Scaffolding template README advertises non-existent `/health` route** (`dx-tooling.md` M5).
      `rume-cli/templates/README.md.tpl`: Documents `GET /health`, but scaffolded `routes.rs.tpl` only registers `/users` and `/posts`. Align template routes with documented operational endpoints.
- [x] **DX m2 — Inconsistent JSON CLI flags across inspection commands** (`dx-tooling.md` m2).
      `crates/cli/src/cli.rs:40–60`: `env check` accepts `--all`, `doctor` accepts `--format json`, `config check` lacks JSON. Standardize on `--json` across all CLI diagnostic commands.
- [x] **DX m3 — CLI generators use naive first-letter `capitalize` rather than `pascal_case`** (`dx-tooling.md` m3).
      `crates/cli/src/commands/generate/mod.rs:30–50`: Generating `user_profile` produces `User_profile` instead of `UserProfile`. Use `rume::helpers::pascal_case`.
- [x] **DX m7 — Generated project `tests/smoke.rs.tpl` tests throwaway inline router** (`dx-tooling.md` m7).
      `rume-cli/templates/tests/smoke.rs.tpl`: Test constructs a dummy router instead of testing the generated app. Update smoke test template to exercise the actual scaffolded router.
- [x] **DX m8 — Terminal UI `Sticker` calculates box padding using byte length instead of display width** (`dx-tooling.md` m8).
      `crates/cli/src/ui/sticker.rs:40–60`: Non-ASCII characters in terminal banners skew border alignment. Use `unicode-width` for visual column calculations.

### Group K — Testing Oracle & Deterministic Clocks (solve together)

- [x] **Testing M1 — Happy-path test bias across newly introduced subsystems fails to verify failure modes** (`testing.md` M1).
      `crates/core/tests/`: Subsystems (outbox, jobs, resilience, sagas, coordination) test only success paths. Add test cases for handler panics, broker down, timeout triggers, and delimiter collisions.
- [x] **Testing M3 — Widespread non-deterministic wall-clock sleeps across async tests** (`testing.md` M3).
      `crates/core/tests/coordination_suite.rs`, `resilience_suite.rs`: Tests use `tokio::time::sleep(Duration::from_millis(30))`, causing flakiness in CI. Use `tokio::time::pause()` to fast-forward simulated virtual clocks deterministically.
- [x] **Testing m1 — `ModuleHarness` lacks health check inspection and error-path coverage** (`testing.md` m1).
      `crates/core/src/module/harness.rs:35–60`: Harness tests route execution but does not expose registered module health checks. Add `harness.health_check()` assertion helper.
- [x] **Testing m2 — Process environment mutation in `env_and_config_loading.rs` is not hermetically restored** (`testing.md` m2).
      `crates/core/tests/env_and_config_loading.rs:50–75`: Test mutates process environment variables without restoring prior state. Use RAII env-guard or isolated subprocess runners.
- [x] **Testing m3 — Config unit tests use hardcoded `/tmp` paths rather than isolated `tempfile::tempdir()`** (`testing.md` m3).
      `crates/core/src/config/loader.rs:85–110`: Concurrent tests sharing `/tmp/app.toml` risk race conditions. Replace with `tempfile::tempdir()`.

---

## P3 — Minor Hardening & Subsystem Cohesion

### Group L — Subsystem Cohesion & Doc Alignment (solve together)

- [x] **Architecture m1 / DX m1 — Orphaned `ops_router` duplicates `health_routes`** (`architecture.md` m1, `dx-tooling.md` m1).
      `crates/core/src/app/shutdown.rs:50–80`: Deprecate or remove `ops_router` in favor of canonical `health_routes`.
- [x] **Architecture m2 — Misplaced tracing and ops helpers in `shutdown.rs`** (`architecture.md` m2).
      `crates/core/src/app/shutdown.rs:20–45`: `init_tracing` and `ops_router` live in `shutdown.rs`. Move to `crates/core/src/app/observability.rs` and `crates/core/src/routing/health.rs`.
- [x] **Architecture m3 — Stale crate architecture doc comment in `crates/core/src/lib.rs`** (`architecture.md` m3).
      `crates/core/src/lib.rs:1–6`: Doc comment claims features like auth/jobs/storage live in external crates, whereas they are now built-in. Update doc comment to reflect current architecture.
- [x] **Architecture m4 — `init_tracing()` panics on already-initialized subscriber** (`architecture.md` m4).
      `crates/core/src/app/shutdown.rs:30–45`: Returns `()` and panics if called twice. Return `Result<(), TryInitError>` or provide `try_init_tracing()`.
- [x] **Correctness m1 / DX m6 — Stale "extractor" docstrings and unused phantom type parameter on `ValidatedParam`** (`correctness.md` m1, `dx-tooling.md` m6).
      `crates/core/src/routing/validated_param.rs:1–30`: Docs describe `ValidatedParam` as a request extractor, but it is now applied via `ParamValidationLayer`. Update docstrings and remove unused phantom types.
- [x] **Enterprise m1 — `MetricsService` leaks `in_flight` gauge on cancelled request futures** (`enterprise-reliability.md` m1).
      `crates/core/src/metrics/layer.rs:65–85`: If client disconnects mid-flight, future drop skips decrement. Wrap gauge increment in an RAII drop guard.
- [x] **Enterprise m2 — `OutboxRelay` lacks retry backoff** (`enterprise-reliability.md` m2).
      `crates/core/src/outbox/relay.rs:115–135`: Immediate retry burns attempt budget during prolonged downstream outages. Add exponential backoff.
- [x] **Enterprise m3 — `MemoryOutboxRepository::fetch_pending` dispatches messages in random hash order** (`enterprise-reliability.md` m3).
      `crates/core/src/outbox/repository.rs:50–70`: Pending messages are pulled from a hash map without ordering by creation timestamp. Sort by `created_at` timestamp.

---

## Counts

| Priority Tier | Findings Count | Severity Breakdown |
|---|---|---|
| **P0 (Critical)** | 8 items | 5 Critical (C), 3 Major (M) |
| **P1 (Major)** | 14 items | 10 Major (M), 4 Minor (m) |
| **P2 (Robustness & DX)** | 15 items | 9 Major (M), 6 Minor (m) |
| **P3 (Hardening & Polish)** | 8 items | 8 Minor (m) |
| **Total** | **45 items** | **5 Critical (C), 22 Major (M), 18 Minor (m)** |
