# Coordination & Multi-Tenancy Review — v04

Lens: **4 — Coordination & Multi-Tenancy**. Whole-document mode. Subject current as of `HEAD` (`cf82559..HEAD` since v03). Docker available; test suites executed locally (see Evidence notes under each finding and in §Verdict).

## Summary

The v03/v03-diff remediation wave genuinely landed its core fixes: PostgreSQL coordination is now database-clock authoritative (`clock_timestamp()` in every lock/leader SQL statement), Redis multi-key scripts are hash-tagged, tenant cache keys are length-prefixed, the Redis quota store uses a sorted-set design, and cross-tenant job/idempotency isolation is in place. This re-review found **no Critical findings**, but **8 Major findings** that block a safe multi-tenant deployment: the tenant-aware rate-limit lookup reads a request extension type the framework never inserts (rendering per-tenant limiting inert and leaving an unbounded header-rotation bypass), tag invalidation can silently no-op after short-TTL writes (stale reads), tenant quota accounting drifts below reality under tag invalidation, `Cache::clear()` issues `FLUSHDB` against the shared Redis logical database, the cache's default `rume:` prefix collides with every other subsystem namespace (including auth sessions), and PostgreSQL `try_acquire` diverges from the Redis election contract for the incumbent candidate. None require redesign — each is a targeted fix — but together they mean the multi-tenant and coordination guarantees this lens exists to assure are **not yet met**.

---

## Prior decisions

Mandatory context read in full: `v03/coordination-multi-tenant.md`, `v03/diff/v01/coordination-multi-tenant.md`, `v02/enterprise-reliability.md` (M3, M6, m4), `v02/security.md` (C2, M1, M2), `v03/todo.md` (Groups E, J, P2), `v03/diff/v01/todo.md` (Group C), `v02/todo.md` (coordination items). Classifications per §3.2 of the plan. Items introduced by the v03 remediation are classified as new coverage (NOTE-NEW → findings `Mn`/`mn`), not as dispositions of old IDs. `v02` coordination items all map 1:1 onto the v03 findings below (Group D / P2 / security-groups), so they are disposed with the same verdicts.

### v03 report (`docs/reports/reviews/v03/coordination-multi-tenant.md`)

- **v03 C1 — Split-brain / lock theft via client wall-clock skew (PostgreSQL coordination)** — **REAFFIRMED**. `current_epoch_millis()` is gone from `coordination/postgres.rs`; every comparison in `lock_acquire.sql:2,7`, `lock_renew.sql:2,3`, `leader_acquire.sql:2,6`, `leader_renew.sql:2,3`, and the prune/get_leader statements (`postgres.rs:60, 177, 255`) uses `(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT`. Executed: `cargo test -p rume --all-features --test containers -- postgres::test_postgres_leader postgres::test_postgres_distributed_lock` → 3 passed.
- **v03 C2 — Stale leader re-acquisition fails to increment election term** — **REAFFIRMED**. The `CASE WHEN candidate_id = EXCLUDED.candidate_id THEN term` path no longer exists; `leader_acquire.sql:5` now sets `term = nextval('rume_leader_term_seq')` unconditionally, so every acquisition — including crash re-acquire — advances the term. Containers test asserts strictly-greater terms (`containers/postgres.rs:657-670`); suite passes. (The *consequence* of keeping `OR rume_leaders.candidate_id = $2` in the same statement is a different defect — filed as **M6**; the term-monotonicity guarantee C2 asked for now holds.)
- **v03 C3 — Redis Lua scripts break under Cluster (missing `{...}` hash tags)** — **REAFFIRMED**. Verified every multi-key script: lock acquire/fence (`coordination/redis.rs:58,62,89-97`), leader/term (`redis.rs:240,244,271-281`), idempotency store (`jobs/redis.rs:196,229-247`), quota store (single-key, hash-tagged: `quota.rs:153-156`). All tag identifiers share the `{...}` slot; Redis cache tag invalidation is now single-key `SMEMBERS` + per-key `DEL` (`cache/redis.rs:217-233`) — no cross-slot multi-key commands remain. Executed: containers coordination suites pass.
- **v03 C4 — Cross-tenant job / idempotency collisions** — **REAFFIRMED**. `rume_jobs.tenant_id` column + compound partial unique index `(COALESCE(tenant_id,''), idempotency_key)` (`jobs_schema.sql:12,26`); `scoped_idempotency_key` length-prefixes tenant scope (`jobs/reliability.rs:66`); `JobWorker` computes `effective_key` (`jobs/worker.rs:402`); `RedisIdempotencyStore` keys are hash-tagged (`jobs/redis.rs:195-197`). Executed: `test_redis_idempotency_store` passes. (Job-side verification beyond this belongs to Lens 3.)
- **v03 M1 — `TenantScopedCache` quota leak on TTL expiration (permanent lockout)** — **REAFFIRMED (Redis path only)**. `RedisTenantQuotaStore` now tracks members in a sorted set scored by expiry ms; every `record_key` prunes `-inf..now` then `ZCARD`s (`quota.rs:199-209`); failed `set_json_with_tags` rolls back via `quota.remove_key` (`tenant/cache.rs:124-128`). Executed: `test_redis_cluster_wide_tenant_quota_coordination` passes. **However**, two residual defects in the same subsystem are filed as new findings: permits released for the wrong members on tag invalidation (**M5**), and the in-memory quota store never prunes on TTL at all (**m2**).
- **v03 M2 — Monotonic memory leak in Redis tag sets from expired TTL keys** — **CHALLENGED (defective remediation)**. The remediation (`cache/redis.rs:156-163`) `PEXPIRE`s each tag set to *this write's* TTL + 1h on every tagged write, **unconditionally lowering** the TTL. Any later short-TTL (or any TTL'd) write shrinks the tag set below earlier longer-lived members; when it expires, `SMEMBERS` returns nothing and `invalidate_tag` silently deletes nothing → stale data served indefinitely. It also severs invalidation for the tenant's *non-expiring* (`ttl=None`) keys whenever any TTL'd key is written to the same tag. The leak half of M2 is closed; the invalidation-correctness half is broken by this very fix. Filed as new finding **M4**; the claimed verification (`containers/redis.rs:242-280`) covers only quota coordination, not tag-set lifetime — the shrink path is untested.
- **v03 M3 — Delimiter collision in `RedisLeaderElector` candidate IDs** — **REAFFIRMED**. Length-prefixed encoding `len:id:term` on acquire (`coordination/redis.rs:274`), matching prefix checks in `renew`/`resign` (`redis.rs:327, 411`), and a `split_once` + bounds-checked parse in `get_leader` (`redis.rs:376-381`) — safe for multibyte IDs (byte lengths, checked slicing, no panic path: out-of-bounds `len` fails `rest.len() > len`; mid-codepoint slice cannot occur because the separator `:` is ASCII and `len` is the exact byte length of the candidate). Executed: `test_redis_leader_election_failover_and_terms` passes.
- **v03 M4 — Global write lock / O(N) eviction in `RateLimiter::check`** — **REAFFIRMED (with caveats)**. The inline `clients.retain()` is gone; `check` is O(timestamps of the key); state is 16 sharded `RwLock<BTreeMap>` (`rate_limiter.rs:44-77`); `prune_expired()` extracted (`rate_limiter.rs:113-123`) and a spawn helper exists (`rate_limiter.rs:126-135`). Caveats: the helper is never wired by the framework (see **m5**), and the tenant-keyed path is broken (see **M1**).
- **v03 M5 — No multi-tenant resource isolation in rate limiting and database pools** — **CHALLENGED (incomplete and partially ineffective remediation)**. Rate-limit keying was added via `x-tenant-id` and XFF first-IP (`rate_limiter.rs:189-207`) and via request extensions — but the extension lookup reads `crate::tenant::TenantContext` (`rate_limiter.rs:191`), a type `TenantLayer` **never inserts** (it inserts `Tenant`, `layer.rs:86`), so the extension path is dead and the attacker-controllable `x-tenant-id` header is the only tenant signal — trivially rotated for unlimited buckets. The DB-pool half of the finding was never delivered at all: `src/persistence/` contains zero tenant awareness (no schema-per-tenant, no RLS, no session variables — verified by exhaustive grep: no `tenant` matches under `src/persistence/`). Pool quota reservations do not exist. Filed as new finding **M1**; the pool isolation gap remains open and is restated in the Verdict, not re-filed (same finding, still unmet).
- **v03 M6 — `HeaderTenantResolver::add_tenant` silently overwrites slugs / precedence shadows vanity slugs** — **CHALLENGED (partial remediation)**. `try_add_tenant` rejects same-namespace collisions (`resolver.rs:54-83`, `TenantError::Conflict` → 409; test `test_header_tenant_resolver_slug_conflict_rejection`, executed: passes). But (a) **cross-namespace** collisions are undetected — a tenant whose `id` equals another tenant's `slug` (or `id` vs `id` is checked, never `id` vs existing slugs / slug vs existing ids) passes both checks and is silently shadowed at resolution time by ID precedence; (b) `SubdomainTenantResolver::add_tenant` still does a bare `insert` with no conflict check at all (`resolver.rs:160-165`); (c) `add_tenant` degrades conflicts to a `tracing::warn!` and continues boot (`resolver.rs:87-92`). Filed as new finding **M7**. (The v02 M2 disposition — separate `by_id`/`by_slug` maps — remains correct as far as it goes; REAFFIRMED for v02 M2.)
- **v03 m1 — Subdomain resolver: case-insensitive hosts / IPv6** — **REAFFIRMED**. Host lowercased per RFC 7230 §5.4; bracketed IPv6 literals parsed before port stripping (`resolver.rs:182-192`); test `test_subdomain_tenant_resolver_case_insensitivity` passes. Subdomain labels still may carry leading/trailing hyphens (`resolver.rs:209-212`) — cosmetic DNS-hygiene note, not filed.
- **v03 m2 — `RedisRateLimiter` misclassifies backend errors as `CircuitOpen`** — **REAFFIRMED**. `ResilienceError::Backend(String)` added (`resilience/error.rs:27`); pool acquisition and script failures map to it (`resilience/redis.rs:60, 104`); `From` maps it to HTTP 500 internal with a static Problem Details body (`resilience/error.rs:34`, `src/error.rs` — diagnostic kept server-side, no client leak).
- **v03 m3 — `LockGuard` / `LeaderLease` lack expiration telemetry** — **REAFFIRMED (with a new defect filed)**. `expires_at: Option<Instant>`, `with_expires_at()`, `is_expired()`, `time_remaining()` exist on both types (`coordination/lock.rs:23-55`, `coordination/leader.rs:17-49`); all four acquire/renew paths populate them; `get_leader` sets `expires_at: None`. The telemetry itself is derived from the **host** monotonic clock, not the DB clock that actually governs the lease — expired-by-DB leases can read as valid (see new **m4**).
- **v03 m4 — Unused global sequence burn in lock conflict path** — **REAFFIRMED**. `lock_acquire.sql:5` now uses `fencing_token = EXCLUDED.fencing_token` — exactly one sequence increment per statement execution; no double-burn, and a rejected acquire burns exactly the `VALUES` nextval.
- **v03 m5 — Expired rows in `rume_locks` / `rume_leaders` retained indefinitely** — **REAFFIRMED**. `idx_rume_locks_expires_at` / `idx_rume_leaders_expires_at` added (`locks_schema.sql:3`, `leader_schema.sql:3`); indexed `prune_expired()` on both backends (`postgres.rs:59-65, 176-182`) using `clock_timestamp()`. Caveat: `prune_expired()` remains a manual API — nothing in the framework schedules it; table bloat is bounded only by operator action. Accepted as remediated (it was filed as "retained until next access / no index or cleanup API", both now present).

### v03/diff/v01 report (`docs/reports/reviews/v03/diff/v01/coordination-multi-tenant.md`)

- **diff M1 — `RedisIdempotencyStore::release` ignores `COMPLETED`, breaking the container suite** — **REAFFIRMED**. `release`/`release_with_token` now delete `IN_PROGRESS`, `IN_PROGRESS:<token>` and `COMPLETED:*` values (`jobs/redis.rs:361-376`). Executed: `test_redis_idempotency_store` passes, including the `release → New` assertion that failed before.
- **diff M2 — `leader_acquire.sql` preserves election term on unexpired re-acquire (fencing loophole)** — **CHALLENGED (partial remediation; residual filed as new finding)**. The old-term hole is closed: `e2dfb9e` made `DO UPDATE … SET term = nextval(...)` unconditional, so a crashed candidate re-acquiring within its TTL now receives a **new, strictly greater** term (commit `e2dfb9e`, verified in current source `leader_acquire.sql:5`). However the remediation **retained the exact `WHERE … OR rume_leaders.candidate_id = $2` clause** the fix prescribed removing ("Require `try_acquire` to succeed only when the previous lease has expired"). Consequences: the Redis backend returns `None` for the incumbent's re-acquire while Postgres **grants** it (extended lease, bumped term) — a cross-backend semantic divergence — and an incumbent that calls `try_acquire` mid-duty self-invalidates every fencing token it has issued (downstream storage rejects its in-flight writes). Filed as new finding **M6**.
- **diff m1 — Duplicate `nextval` burn (two sequence values per acquisition)** — **REAFFIRMED**. Still present and now **unconditional**: `leader_acquire.sql:2` (`VALUES … nextval('rume_leader_term_seq')`) and `leader_acquire.sql:5` (`DO UPDATE … term = nextval('rume_leader_term_seq')`) — exactly two sequence increments per acquired conflict, one per rejected acquire. Fix remains `term = EXCLUDED.term` in `DO UPDATE`. The v03/diff ledger marks this `[x]`; the code shows it was never applied. Monotonicity is unaffected (gaps are harmless); graded m-class, no data risk.
- **diff m2 — `RateLimitService` ignores `TenantContext` extensions** — **CHALLENGED (defective remediation)**. The remediation *is* the defect: `rate_limiter.rs:189-198` reads `req.extensions().get::<crate::tenant::TenantContext>()`, but `TenantLayer` inserts `crate::tenant::Tenant` (`layer.rs:86`) — `TenantContext` (the `TenantContext<T>(pub T)` extractor newtype, `model.rs:40`) is never in extensions, so the lookup always yields `None` and the code silently falls back to the raw, client-controlled `x-tenant-id` header. The finding m2 reported is therefore **unfixed as shipped**; the intended path is dead code. Filed as new finding **M1**.
- **diff m3 — `RateLimiter` global write lock, no background eviction** — **REAFFIRMED (improved, still open)**. The single global `RwLock<BTreeMap>` is now 16 shards and the O(N) eviction is gone from `check`, but the exclusive write lock per request remains (sharded, not eliminated) and `spawn_background_eviction()` has **no call sites anywhere** — expired-entry accumulation in default deployments is unbounded (see **m5**).

### NOTE-NEW (areas not covered, or new defects introduced/exposed by remediation)

- **M1** — Rate-limiter tenant lookup reads the wrong extension type; extension path dead; header rotation bypass (defective remediation of v03 M5 / diff m2).
- **M2** — `RedisCacheBackend` default prefix `rume:` collides with every other subsystem's namespace (auth sessions, locks, idempotency, leaders, jobs heartbeats, rate-limit windows, quota ZSETs) — cross-subsystem key overwrite.
- **M3** — `Cache::clear()` executes `FLUSHDB` — wipes the entire Redis logical DB including non-cache Rume state and foreign applications' keys.
- **M4** — Tag-set `PEXPIRE` unconditionally shortens tag lifetimes → tag invalidation silently no-ops; stale reads (defective remediation of v03 M2).
- **M5** — `invalidate_tag` releases quota permits for arbitrary (oldest) members, not the deleted keys → quota under-accounting drifts into tenant quota overshoot.
- **M6** — `leader_acquire.sql` retains `OR rume_leaders.candidate_id = $2` → incumbent re-acquire diverges from Redis; self-fencing of in-flight writes.
- **M7** — Tenant registration admits silent cross-namespace collisions; subdomain resolver overwrites silently; warn-and-continue hides it at boot.
- **M8** — `remember_with_tags` stampede waiters can sleep forever (lost wakeup; `Notified` created after `notify_waiters()` may have run).
- **m1** — Host wall-clock drives the Redis rate-limit and quota window math (skew blurs cluster-wide windows); server `TIME` is one `redis.call` away.
- **m2** — `InMemoryTenantQuotaStore` never prunes on TTL → permanent tenant lockout (v03 M1's original failure mode, retained in the in-memory store).
- **m3** — `RedisTenantQuotaStore::acquire()` mints permanent `lease-<nanos>` members scored `i64::MAX` (never pruned) if the trait's `acquire` is ever called.
- **m4** — `LockGuard`/`LeaderLease::expires_at` derive from the host `Instant`, not the DB clock that governs the lease; `get_leader` reports `expires_at: None`; after a host pause `is_expired()` reads a dead lease as alive.
- **m5** — `RateLimiter::spawn_background_eviction()` is never wired; in-memory limiter grows unbounded in default deployments.
- **m6** — `InMemoryCacheBackend` tag sets grow monotonically; `delete()` never removes tag members.

No area in this lens was previously unreviewed — every subsystem audited here had prior coverage; the NOTE-NEWs are (a) new defects introduced by the remediation itself (M1, M4, M6), (b) unreached halves of prior findings (M2/M3, M7 vs v03 M6/diff m2), or (c) adjacent gaps prior reviews never covered (M8, m1–m6).

---

## Findings

### M1 `RateLimitService` reads a tenant extension type the framework never inserts — tenant limiting is inert and the header fallback is trivially bypassed

- **Where:** `crates/core/src/resilience/rate_limiter.rs:188-207`; insertion site `crates/core/src/tenant/layer.rs:84-87`; type definition `crates/core/src/tenant/model.rs:40`
- **What it says:**
  ```rust
  let tenant_key = req
      .extensions()
      .get::<crate::tenant::TenantContext>()   // never inserted
      .map(|tc| format!("tenant:{}", tc.id))
      .or_else(|| {
          req.headers().get("x-tenant-id")
              .and_then(|h| h.to_str().ok())
              .map(|t| format!("tenant:{}", t.trim()))
      });
  ```
  `TenantLayer::call` inserts `Tenant` (`req.extensions_mut().insert(tenant)` — the *plain* struct). `TenantContext` is a separate newtype used only as an extractor.
- **Why it's a problem:**
  1. The extension path can never succeed — per-resolved-tenant rate limiting silently does nothing, even for subdomain-routed apps where the framework holds an authoritative tenant.
  2. The fallback trusts the raw client header `x-tenant-id`. A client rotating `x-tenant-id: <random>` per request gets a fresh bucket each time → **rate limiting fully defeated** for any app not stacking `TenantLayer`, and for `TenantLayer` apps too (the lookup above still misses).
  3. The existing test (`tests/integration/resilience.rs:75-128`) never stacks `TenantLayer`, so nothing catches this — the remediation for v03 M5 / diff m2 is dead on arrival and untested on its intended path.
- **Suggested fix:** Look up the type actually inserted: `req.extensions().get::<crate::tenant::Tenant>().map(|t| format!("tenant:{}", t.id))`. Drop the unverified `x-tenant-id` header as a rate-limit key, or gate it explicitly (e.g. `RateLimitConfig::trust_tenant_header: bool`, documented as “only behind a trusted proxy that strips client-supplied values”). Fall back to the parsed first XFF hop (existing behavior) and document that XFF trust requires proxy validation (Lens 5 overlap, one line). Add an integration test stacking `TenantLayer::optional` + `RateLimitLayer` proving two tenants get independent buckets and a header-only client cannot mint unlimited buckets.
- **Evidence:** Static — all three files read in full (`rate_limiter.rs:184-240`, `layer.rs:75-104`, `model.rs:35-73`); exhaustive grep confirms the only extension lookups of this kind are `auth/layer.rs:171` (`AuthUser`, correct type). Executed: `cargo test -p rume --all-features --test integration` → **166 passed, 0 failed** — includes rate-limit tests, all of which exercise the IP path only. Not executed: no test exists for the tenant-extension path (that is the finding).
- **Root cause:** remediation wrote the fix against the extractor's type name instead of the type actually stored in extensions — i.e., fix-by-name, not fix-by-read. Same missing rule as diff m2's “fixed” comment.

### M2 `RedisCacheBackend` default prefix `rume:` collides with every other subsystem namespace — cache writes can overwrite auth sessions and coordination state

- **Where:** `crates/core/src/cache/redis.rs:42, 50` (`prefix: "rume:"`); colliding namespaces: `auth/redis.rs:39` (`rume:session:`), `coordination/redis.rs:39, 221` (`rume:lock:`, `rume:leader:`), `jobs/redis.rs:172` (`rume:idempotency:`), `jobs/redis.rs:43, 51` (`rume:jobs:heartbeats`), `resilience/redis.rs:38` (`rume:ratelimit:`), `tenant/quota.rs:143` (`rume:quota:`), `cache/redis.rs:69-71` (`tag_set_key` → `rume:tag:*`)
- **What it says:** `RedisCacheBackend::new/from_pool` default `prefix` to `"rume:"`, so cache key `"session:abc"` is stored at `"rume:session:abc"` — byte-identical to the session store’s key for session `abc` (`auth/redis.rs:39` + `"abc"`).
- **Why it's a problem:** The cache and the security/coordination subsystems share one flat logical namespace distinguished only by app-chosen key strings. Any application caching a key shaped like `<subsystem>:<id>` — or caching attacker-influenced content at such keys — silently **overwrites or reads** another subsystem’s records: destroying auth sessions (integrity + forced re-login), corrupting idempotency records (duplicate side effects on retry), or clobbering lock/leader keys (mutual exclusion degraded). The cache backend offers no reserved sub-namespace, so the collision is silent — the `SET` succeeds, the victim just vanishes on next read. Notably, auth session loss is not only availability: a poisoned `rume:session:<id>` value parsed by `RedisSessionStore::load` can forge a session for any *predictable* session id an attacker can pre-pollute, elevating this from nuisance to auth-state corruption.
- **Suggested fix:** Change the default (and `from_pool`) cache prefix to `rume:cache:` (and tag sets to `rume:cache:tag:`), with the same for `with_prefix` docs; keep a `why:` note at the constant citing this finding (there is no scenario where cache sharing the global `rume:` root is safe). Existing deployments must FLUSHDB-or-migrate the `rume:` root or accept stale cross-keys — call it out in the changelog. Add a unit test asserting `RedisCacheBackend::new("redis://…").pool()` uses a prefix disjoint from the other subsystems’ prefixes.
- **Evidence:** Static — all prefix constants read at the cited lines; exhaustive grep for `"rume:` across `src/` shows no namespace reserved for the cache. Executed: none needed to establish the byte-collision (string equality is decidable by inspection); container suite exercised only the happy path (`test_redis_cache_*` pass). Not executed: end-to-end session-poisoning reproduction against a live Redis (not required to land the fix; would be the acceptance test).
- **Root cause:** no reserved namespace rule — every subsystem picked its own `rume:<x>:` root and the cache was allowed to squat on the shared root. This and **M3** are one missing rule: *the cache must not operate on unqualified keys in a shared namespace.*

### M3 `Cache::clear()` executes `FLUSHDB` — one cache call wipes sessions, locks, idempotency records and foreign tenants on the shared Redis logical DB

- **Where:** `crates/core/src/cache/redis.rs:245-254` (via `Cache::clear`, `typed.rs:238-243`)
- **What it says:**
  ```rust
  let mut conn = self.pool.get().await…;
  redis::cmd("FLUSHDB").query_async::<()>(&mut *conn).await…
  ```
- **Why it's a problem:** `Cache::clear()` is documented as “Evicts all entries from the cache.” Its implementation deletes **every key in the Redis logical database** — not the cache’s namespace, not `rume:cache:*`, not even `rume:*`: also any other application’s keys co-located on the instance. For Rume itself it deletes auth sessions (`rume:session:*` → every user forcibly logged out), distributed locks and leader leases (`rume:lock:*`, `rume:leader:*` → mutual exclusion silently gone for in-flight critical sections), idempotency records (`rume:idempotency:*` → completed-side-effect keys forgotten → duplicate payment/email/job dispatch on retry), heartbeats (`rume:jobs:heartbeats`), quota ZSETs (`rume:quota:*`) and rate-limit windows. On a shared production Redis — the normal small-deployment shape — this is a subsystem-wide data-loss operation behind a method whose name promises a cache flush. The call is explicit, but operators routinely wire cache flushes into deploys and admin panels; the method cannot be used safely as written.
- **Suggested fix:** Implement `clear()` as `SCAN`/`UNLINK` over the cache’s own reserved prefix only (after **M2** lands: `rume:cache:*`), preferably `UNLINK` in pipelined batches; if a whole-DB flush is ever needed, expose it as a separate, explicitly named, documented operation (`flush_database()`) that errors on non-dedicated DB indexes, or delete the method. Do not ship `FLUSHDB` behind `cache.clear()`. Add a container test proving sessions survive `cache.clear()`.
- **Evidence:** Static — `redis.rs:245-254` read; Redis cluster semantics not required for the finding (logical-DB FLUSHDB is universal). Executed: container suite ran but never calls `clear()` (grep shows zero call sites in tests or src outside the backend itself — blast radius proven, not triggered). Not executed: destructive flush against the shared test Redis — deliberately not run; the defect is evident from the command text.
- **Root cause:** same missing rule as **M2** — cache operating on unqualified scope in a multi-tenant shared namespace.

### M4 Tag-set `PEXPIRE` unconditionally *shortens* the tag lifetime — tag invalidation silently no-ops; stale data served until key TTL

- **Where:** `crates/core/src/cache/redis.rs:148-163` (set path), `208-237` (invalidation path); tag key format `cache/redis.rs:69-71`; secondary consumer `tenant/cache.rs:146-155`
- **What it says:**
  ```rust
  cmd.arg(&k).arg(&value);
  if let Some(ttl) = ttl { cmd.arg("PX").arg(ttl.as_millis() as u64); }
  // … SET succeeds …
  if let Some(ttl) = ttl {
      let tag_ttl_ms = (ttl.as_millis() as u64).saturating_add(3_600_000);
      let _ = redis::cmd("PEXPIRE").arg(&tk).arg(tag_ttl_ms).query_async::<()>(&mut *conn).await;
  }
  ```
  `PEXPIRE` sets an absolute TTL each time — it never refuses to lower an existing one.
- **Why it's a problem:**
  1. **Unequal member lifetimes:** write key A with `ttl = 7 days` and tag `t` → tag set TTL ≈ 7 d + 1 h. Later write key B (`ttl = 60 s`, same tag) → tag set TTL **reset to ≈ 1 h**. After ~1 h the tag set is evicted while A lives for 6 more days. `invalidate_tag("t")` then finds no set, deletes nothing, returns 0, and the caller reports success — **A is never invalidated and serves stale bytes until its own TTL**. This is a silent, correctness-breaking regression introduced by the v03/diff M2 remediation.
  2. **Mixed TTL / no-TTL members:** write key N with `ttl = None` (never expires) tagged `t` — no `PEXPIRE` is issued, fine. Later write any TTL'd key tagged `t` → `PEXPIRE` now condemns the tag set; N’s tag invalidation path is destroyed ~1 h later.
  3. The tag set is *supposed* to be the invalidation index; the remediation inverted the invariant — the set may now die while live members remain. The container test does not exercise this (no test writes two same-tag keys with different TTLs and then invalidates after the shorter one expires).
- **Suggested fix:** Only ever *extend* the tag set, never shorten, and never expire it while a non-expiring member exists. Minimal form — single Lua script on write:
  ```lua
  -- keys: tag_key; argv: new_ttl_ms (or -1 for "persist")
  if tonumber(ARGV[1]) < 0 then redis.call('PERSIST', KEYS[1]); return end
  local cur = redis.call('PTTL', KEYS[1])
  if cur == -2 or cur < tonumber(ARGV[1]) then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end
  ```
  i.e. skip when the member is non-expiring (`PERSIST` the set), otherwise keep the max. Better (larger change): ZSET tag index scored by member expiry + `ZREMRANGEBYSCORE`, mirroring the quota-store design. Add a container test: set A (ttl 7d, tag t), set B (ttl 60s, tag t), wait > tag-set lifetime, `invalidate_tag("t")` must still delete A; and set N (no TTL, tag t) + set B (ttl 60s) must leave N invalidatable forever.
- **Evidence:** Static — lines cited; the algorithm is plainly shortening on write order (no `PTTL` guard anywhere). Executed: `test_redis_cache_tag_invalidation` passes (uniform `ttl=None` members — the safe case only); `test_redis_cluster_wide_tenant_quota_coordination` passes (no tag use). Not executed: a live Redis reproduction of the shrink (the flaw is decidable from the command sequence; the container harness could run it unchanged, but the fix and its tests belong to the remediation pass).
- **Root cause:** TTL-condemning the index instead of the members — the tag set’s lifetime was coupled to the *latest writer* instead of the *longest-lived member*. Same missing rule as v03 M2: an invalidation index must outlive its members.

### M5 `TenantScopedCache::invalidate_tag` releases quota permits for the *oldest* tracked keys, not the deleted ones — tenant quotas drift into overshoot

- **Where:** `crates/core/src/tenant/cache.rs:146-155`; member removal `crates/core/src/tenant/quota.rs:250-275` (`release` → `ZRANGE 0 0` + `ZREM`), pruning `quota.rs:199-209, 290-304`; count consumed at `cache.rs:103-111, 167-174`
- **What it says:**
  ```rust
  // tenant/cache.rs
  let count = self.cache.invalidate_tag(&scoped).await?;   // usize — which keys died? unknown
  if let Some(ref quota) = self.quota_store {
      for _ in 0..count { let _ = quota.release(tenant_id).await; }
  }
  // tenant/quota.rs (Redis)
  local oldest = redis.call('ZRANGE', KEYS[1], 0, 0) … ZREM(oldest[1])
  ```
  `release` discards the **lowest-score (soonest-expiring) member** of the tenant’s sorted set — arbitrary with respect to which keys the tag invalidation actually deleted.
- **Why it's a problem:** the sorted set and the cache hold *different* member identities (quota tracks the raw user `key` — e.g. `"p1"` — while the backend’s tag members are prefixed full keys — e.g. `"rume:p1"`), so positional `release` cannot target the right entries. Concretely, for one `invalidate_tag` deleting N keys:
  1. `release` removes the N lowest-score *live* keys from the sorted set → those keys remain cached but are now **untracked** (their permits freed while they still occupy memory).
  2. The N keys actually deleted are usually **left in the sorted set** with future expiry scores. No call removes them until their original TTL elapses — at which point `record_key`/`count` prune them via `ZREMRANGEBYSCORE`, decrementing the count a **second** time for a key whose permit was already released at invalidation.
  3. Net effect: tracked count drifts **below** live count, monotonically, by up to N per invalidation. The tenant eventually caches far more than `max_keys_per_tenant` while the quota reports headroom — the isolation bound (noisy-neighbor memory protection) is silently defeated.
  4. Same flaw in the in-memory store (`release` decrements the counter blindly, `quota.rs:85-94`), and the same *count* feeds it, so single-node deployments drift identically.
  The in-flight `count` also includes tag-set members that were already dead (SMEMBERS size ≠ live deletions — `cache/redis.rs:223`), inflating the misallocation on every invalidation.
- **Suggested fix:** carry the deleted-member list instead of a bare count:
  1. `CacheBackend::invalidate_tag` returns `Result<Vec<String>, CacheError>` (members actually `DEL`’d; for Redis, the `SMEMBERS` list filtered to DEL results; for in-memory, the removed intersection with live entries).
  2. `Cache::invalidate_tag` strips its own configured prefix and returns the **scoped** keys (it owns `format_key`).
  3. `TenantScopedCache` records quota members by scoped key (change `record_key`/`remove_key` call sites at `cache.rs:104, 127, 140` to `Self::scoped_key(tenant_id, key)`) and, on tag invalidation, calls `quota.remove_key(tenant_id, &scoped).await` for each returned key — exact `ZREM`s, no guessing, no drift.
  Breaking change to the trait, but small and mechanical; alternatively document `release` as a no-op for invalidations and accept a one-directional leak (worse). Add a container test: set 4 keys (quota 4), tag-invalidate 2 (the two with the *nearest* expiry), assert `active_key_count == 2` and the remaining two are exactly the untagged/still-cached ones.
- **Evidence:** Static — full path traced across the three layers; member identities and scoring verified at the cited lines. Executed: `tenant_scoped_cache_tag_isolation_and_quota_bounds` (in-memory) and `test_redis_cluster_wide_tenant_quota_coordination` (live Redis) both pass — neither exercises tag-invalidation quota attribution, so the defect is untested; `invalidate_tag` count semantics confirmed at `cache/redis.rs:223` (`keys.len()`, not live deletions). Not executed: a failing-before reproduction script (belongs to the remediation pass; the invariant violation is direct from the code).
- **Root cause:** count-based permit release where the keys’ identities were available — the API chose `usize` over the member list. Same bounded-context boundary bug family as M4 (index and payload accounted separately, never reconciled).

### M6 PostgreSQL `try_acquire` retains `OR rume_leaders.candidate_id = $2` — incumbent re-acquire diverges from Redis and self-fences in-flight writes

- **Where:** `crates/core/src/persistence/sql/postgres/leader_acquire.sql:7-8`; caller `crates/core/src/coordination/postgres.rs:186-217`; counterpart semantics `crates/core/src/coordination/redis.rs:249-303` (`if not current then … return nil`)
- **What it says:**
  ```sql
  WHERE rume_leaders.expires_at <= clock_timestamp()…
     OR rume_leaders.candidate_id = $2
  ```
  The same-candidate disjunct lets a live, unexpired incumbent re-`try_acquire` at any time — receiving a **new term** and an **extended lease**.
- **Why it's a problem:**
  1. **Backend divergence:** Redis returns `None` whenever the lease is held, even by the caller (`try_acquire` is election-only; renewal goes through `renew()`). Postgres grants the same call with a bumped term. An application ported between backends, or run against both in staging, observes materially different election semantics from identical code — precisely the inconsistency `v03/diff/v01 M2` called out ("diverging from `RedisLeaderElector`"), and it was kept.
  2. **Self-fencing footgun:** an incumbent that defensively re-acquires leadership mid-duty (a common “confirm before starting a work batch” pattern) gets term N+1 while its in-flight writes still carry term N. Downstream storage applying monotonic fencing **rejects the incumbent’s own legitimate writes** — a self-inflicted availability outage, not corruption, but real. Every `try_acquire` is also a term bump served to no election purpose and burns two sequence values (see prior decision, v03/diff m1).
  3. The clause exists only to implement “idempotent re-acquire”, which `renew()` already provides with correct semantics (term preserved, expiry extended, requires unexpired lease). It duplicates `renew` while silently changing the fencing token — the worst of both.
- **Suggested fix:** Remove `OR rume_leaders.candidate_id = $2`; require expiry for takeover:
  ```sql
  WHERE rume_leaders.expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
  ```
  and set `term = EXCLUDED.term` (also fixes diff m1’s double-burn). Incumbent renewal remains via `renew()` (`leader_renew.sql`, already correct, `RETURNING term` for fencing continuity). A candidate whose lease expired re-acquires legitimately with a new term. Add a container test asserting both backends return `None` for incumbent `try_acquire` during an active lease, and that post-expiry re-acquire yields a strictly greater term on both.
- **Evidence:** Static — SQL and callers read at cited lines; behavioral divergence confirmed against the Redis Lua script (`if not current … return nil`, any held lease rejects). Executed: `test_postgres_leader_election_failover_and_resignation` and `coordination::test_leader_election_lifecycle` pass — neither exercises same-candidate re-acquire, so the divergence is untested (the PG test path only covers expired-takeover and resign-then-takeover). Not executed: a differentiation test across both backends (belongs to remediation).
- **Root cause:** the v03/diff M2 fix corrected term *value* semantics but kept the clause that made `try_acquire` a disguised renewal — election and heartbeat concerns remain conflated in one statement. Same conflation the rest of the codebase (lock vs renew, jobs claim vs heartbeat) deliberately avoids.

### M7 Tenant registration admits silent collisions — cross-namespace ID/slug shadowing, unguarded subdomain overwrite, warn-and-continue at boot

- **Where:** `crates/core/src/tenant/resolver.rs:54-92` (`try_add_tenant`, `add_tenant`), `resolver.rs:160-165` (`SubdomainTenantResolver::add_tenant`); precedence rule `resolver.rs:115-127`; consequence `tests/integration/tenant.rs:122-166` (documented behavior)
- **What it says:**
  ```rust
  // try_add_tenant checks same-namespace only:
  if let Some(existing) = slug_map.get(&tenant.slug) && existing.id != tenant.id → Conflict
  if let Some(existing) = id_map.get(&tenant.id)  && existing.slug != tenant.slug → Conflict
  // id ≠ anyone's slug; slug ≠ anyone's id — never checked.
  // SubdomainTenantResolver: map.insert(tenant.slug, tenant) — no check at all.
  // HeaderTenantResolver::add_tenant: on conflict, tracing::warn! and continue
  ```
- **Why it's a problem:**
  1. **Cross-namespace shadowing:** existing tenant E with `slug = "acme"`; new tenant N with `id = "acme"`, `slug = "other"`. N passes both checks (`slug_map["other"]` absent; `id_map["acme"]` absent), registers, and now `X-Tenant-ID: acme` resolves to N (ID precedence) — E’s vanity slug is **silently captured** at registration time, with no error and no warning. Mirror case (`id` vs existing `slug` of another tenant) likewise passes. This is the exact hijack class of v02 M2 / v03 M6, relocated to the seam between the two namespaces the remediation created.
  2. **Subdomain resolver never got the conflict API:** `SubdomainTenantResolver::add_tenant` unconditionally overwrites `slug → Tenant`. Registering a second tenant with the same slug replaces the first with zero signal — for subdomain-routed SaaS this is tenant capture by configuration order.
  3. **Warn-and-continue:** even same-namespace conflicts detected by `try_add_tenant` are reduced to a log line by `add_tenant`; a provisioning pipeline booting 1,000 tenants can drop one silently (only in logs) while serving the rest — a partial, inconsistent tenant registry presented as healthy. `try_add_tenant` exists precisely for strictness, but the convenient path is the lenient one.
- **Suggested fix:** In `try_add_tenant`, also check the cross-namespace pairs: `id_map.get(&tenant.slug)` (new slug collides with someone’s id) and `slug_map.get(&tenant.id)` (new id collides with someone’s slug) — both must `Err(Conflict)` (or at least `warn!` with tenant ids when shadowing is a supported product feature, but the charter is “explicit”: conflict, not shadow). Give `SubdomainTenantResolver` the same `try_add_tenant` semantics (return `Result`, 409 semantics unchanged) and keep `add_tenant` delegating with a loud `error!` (not `warn!`) or fail-fast for boot provisioning. Document precedence in one line (ID wins ties deterministically — fine, once registration forbids ties). Tests: cross-namespace both directions → `Conflict`; subdomain duplicate → error; `add_tenant` on conflict logs at error level and the tenant is absent.
- **Evidence:** Static — all four code paths read; precedence and registration order verified against the unit tests at `resolver.rs:324-377` (which confirm shadowing is *intended* at resolution but never *rejected* at registration); exhaustive check of the two check blocks confirms the cross pairs are missing. Executed: `cargo test -p rume --all-features --test integration -- tenant` → tenant suites pass (they cover same-namespace conflicts only). Not executed: none needed.
- **Root cause:** conflict validation was implemented per-namespace rather than per-tenant-identity — the registration invariant (“one tenant, one reachable name”) was never stated, so each namespace checked itself and the seam went missing. This and the subdomain overwrite are the same missing rule.

### M8 `remember_with_tags` stampede waiters can block forever — `Notified` created after `notify_waiters()` ran is never woken

- **Where:** `crates/core/src/cache/typed.rs:167-208`; waiter registration `typed.rs:179-185` vs slot-release/notify `typed.rs:199-205`
- **What it says:**
  ```rust
  let waiter = { … if let Some(existing) = flights.get(&full_key) { Some(Arc::clone(existing)) } … };  // mutex dropped here
  if let Some(notify) = waiter {
      notify.notified().await;   // ← created here, outside the mutex
      …
  }
  …
  let to_wake = { flights.remove(&full_key) };   // leader side
  if let Some(notify) = to_wake { notify.notify_waiters(); }
  ```
- **Why it's a problem:** tokio documents that `notify_waiters()` wakes **already-registered** waiters and stores no permit; `Notified` futures *created after* the call never fire (“The `Notified` future is guaranteed to receive wakeups from `notify_waiters()` as soon as it has been created, even if it has not yet been polled.”). Because the waiter creates its `Notified` **after** releasing the `flights` mutex — and the leader can complete `f()` + `set_json_with_tags` + `flights.remove` + `notify_waiters()` entirely within that window — the waiter registers too late and **parks on `notified().await` forever**: the HTTP request never returns (client sees a hung connection until an external timeout kills it; nothing in this path enforces one). The race window is small but real and widens under load (leader CPU-starved between mutex drop and `.notified()` creation). The existing stampede test passes only because the leader sleeps 30 ms, giving waiters ample time to register — it cannot observe the lost wakeup. Moreover, after a *leader failure*, all waiters that do wake re-execute `f()` concurrently (no re-registration) — a thundering-herd on the error path that the singleflight design exists to prevent (worth a note; not claimed as a second defect to keep severity honest).
- **Suggested fix:** Create the `Notified` **inside** the `flights` critical section:
  ```rust
  let waiter = {
      let mut flights = self.singleflight.lock().await;
      if let Some(existing) = flights.get(&full_key) {
          Some(tokio::pin!(existing.notified())) // create before dropping the lock
      } else { … }
  };
  if let Some(notified) = waiter { notified.await; … }
  ```
  (or `Notified::enable()` semantics — but creation order is sufficient for `notify_waiters`). Also: after wake, if the cache is still missing, re-check-and-join or execute with a fresh singleflight slot instead of falling through blindly. Regression test: barrier-controlled “leader finishes before waiter calls notified()” — assert the waiter completes (currently: hangs until test timeout).
- **Evidence:** Static — control flow read end-to-end; ordering of mutex drop vs `notified()` creation is unambiguous; tokio semantics verified against official docs (`tokio::sync::Notify::notify_waiters`: “no permit is stored … notifies all already registered waiters”, and `notified()`: guaranteed wakeups only “as soon as it has been created”). Executed: `cache::cache_remember_stampede_singleflight_protection` passes (leader sleep window masks the race). Not executed: deterministic reproduction of the lost-wakeup (needs an instrumented build or loom; belongs to remediation; the defect does not require execution to diagnose).
- **Root cause:** check-then-register without holding the registration invariant — the same shape as classic lost-wakeup (condition checked, mutex dropped, *then* waiter registered). C10 in v02 was the same class in the outbox relay path.

### m1 Host wall-clock drives Redis rate-limit and quota window math — cross-node clock skew blurs cluster-wide windows

- **Where:** `crates/core/src/resilience/redis.rs:62-65` (`now_ms = SystemTime::now()`), `crates/core/src/tenant/quota.rs:164, 179-185, 279-282` (lease id + prune/score base)
- **What it says:** `now_ms` and the sorted-set scores/prune bounds are computed from each app node’s local `SystemTime::now()` and passed as `ARGV` into single-node Lua scripts.
- **Why it's a problem:** cluster nodes with skewed clocks disagree on which window members are “expired” — a node behind the fleet keeps members longer (inflating counts, fail-closed-ish), a node ahead prunes live members early (deflating counts → **over-admission**: limits exceeded cluster-wide when traffic shifts between skewed nodes). The PostgreSQL side of the codebase was decisively moved to DB time (v03 C1); the Redis side still trusts each writer’s clock. Not a fencing or safety issue (window semantics only), but a silent accuracy defect with trivial remediation.
- **Suggested fix:** Inside the Lua scripts, derive time from the server: `local now = tonumber(redis.call('TIME')[1]) * 1000 + math.floor(tonumber(redis.call('TIME')[2]) / 1000)`. Pass only the window/max/ttl as args. For the quota ZSET, score by server time identically (`redis.call('TIME')` in `record_key` + `count`), eliminating the `now_ms` param entirely. Keep host time only for telemetry.
- **Evidence:** Static — lines cited. Executed: container Redis suites pass (single shared container clock — skew invisible). Not executed: skewed-node reproduction.

### m2 `InMemoryTenantQuotaStore` never prunes on TTL — permanent tenant lockout in the in-memory mode

- **Where:** `crates/core/src/tenant/quota.rs:53-112` (whole store), trait default `quota.rs:26-35`
- **What it says:** `acquire` increments a per-tenant counter `if *count < max_keys`; `record_key` is **not overridden**, so the trait default discards `key`/`ttl` and calls `acquire`; nothing ever decrements on expiry.
- **Why it's a problem:** this is exactly the v03 M1 failure mode — after `max_keys_per_tenant` writes cumulatively, the tenant is permanently refused cached writes even if every key expired — retained in the “testing and single-instance deployments” store (its own doc comment). Test fixtures that set/expire many keys will flake into `QuotaExceeded`; a small production single-node app hits it by Tuesday. Consistency with the Redis store (the fixed path) is also lost — behavior differs across backends.
- **Suggested fix:** Mirror the Redis design in-memory: `HashMap<String /*tenant*/, HashMap<String /*key*/, Instant /*expires_at*/>>`; `acquire`/`record_key` purge expired entries first (or store a `BTreeMap<Instant, String>` plus per-key map); `release` no longer decrements a blind counter. Alternatively restrict the in-memory store to no-TTL semantics and document — but that re-creates the lockout, so the purge is the honest fix. Add a unit test: fill to max with 1-minute TTL, advance `tokio::time::pause()` past TTL, next `set` succeeds.
- **Evidence:** Static — full file read; trait default confirmed to discard `ttl` (`quota.rs:33-35`). Executed: integration tenant suites use this store only at trivial counts (pass). Not executed: exhaustive fixture test (belongs to remediation).

### m3 `RedisTenantQuotaStore::acquire()` mints permanent `lease-<nanos>` members scored `i64::MAX`

- **Where:** `crates/core/src/tenant/quota.rs:158-168` (+ `record_key` score computation `quota.rs:183-186`)
- **What it says:** `acquire` synthesizes `unique_id = format!("lease-{}", now_nanos)` and calls `record_key(…, ttl: None)` → score `i64::MAX`, pruned by nothing, removed by nothing (`ZREMRANGEBYSCORE -inf now` never reaches it).
- **Why it's a problem:** any caller using the trait’s `acquire` (its purpose-built API for “reserve a slot without a key”) leaks an unremovable member per call; at `max_keys` such calls the tenant is permanently quota-exceeded with an invisible leak — debugging requires ZRANGE-archaeology. Currently uncalled by the framework (only `record_key` is used, `tenant/cache.rs:104`), so this is a loaded footgun in a public trait impl, not an active outage.
- **Suggested fix:** Delete `acquire`’s special case: implement it as `record_key` with an explicit TTL, or make `acquire` take `ttl: Duration` (breaking, honest). Cheapest correct: have `acquire` refuse (`Err(CacheError::Backend("acquire requires a key; use record_key"))`) or delegate to a transient reservation that `release` can remove by identity. Simplest of all: remove `acquire` from the trait’s required surface — nothing needs it; keep `record_key`/`remove_key`.
- **Evidence:** Static — lines cited; grep confirms zero call sites of `.acquire(` on quota stores in the codebase. Executed: n/a. Not executed: n/a (dead path).

### m4 `LockGuard`/`LeaderLease::expires_at` derives from the host clock — `is_expired()` certifies dead leases as live after a host pause; `get_leader` reports `expires_at: None`

- **Where:** `crates/core/src/coordination/lock.rs:23-25, 44-55`; `coordination/leader.rs:17-19, 38-49`; population sites `coordination/postgres.rs:93, 211, 244, 270` (`get_leader` sets `None`), `coordination/redis.rs:113, 297, 351`, `cohort` renew path `leader.rs` via `CoordinationError` — and DB-authoritative comparison in `leader_renew.sql:3`
- **What it says:** guards carry `expires_at = Some(Instant::now() + ttl)` — host monotonic, unrelated to the `clock_timestamp()` basis the SQL actually enforces; `get_leader` populates `expires_at: None`, making `is_expired()` vacuously `false` there.
- **Why it's a problem:** the telemetry added by the v03/diff m3 fix tells the operator “2.1 s remain” while PostgreSQL — the authority — may already consider the lease dead (host froze: hypervisor pause, SIGSTOP, long GC; on Linux `Instant` uses `CLOCK_MONOTONIC`, which does **not** count suspend time). An operator/auto-renew loop gating work on `is_expired()` will extend a lease the DB already re-granted to a peer — or skip renewal of one it thinks is fine. `get_leader` returning `expires_at: None` additionally breaks any caller expecting a coherent lease object. Fail-operational in the unsafe direction (thinks it holds), even though the DB still fences the writes.
- **Suggested fix:** populate guard expiry from the same basis the backend uses — either have acquire/renew return the server-computed `expires_at` (add `expires_at_ms` to SQL `RETURNING` — PG: `RETURNING fencing_token, (EXTRACT(EPOCH FROM clock_timestamp())*1000)::BIGINT + $ttl`; Redis: use server `TIME` in the script or `PTTL` round-trip), or document `expires_at` explicitly as “host-local advisory; not for fencing decisions” and have `is_expired()` consult the backend. Minimal viable: document + `get_leader` sets `None` by contract, and renew results overwrite it. The disclaimer is one doc line; the full fix is half a day.
- **Evidence:** Static — constructions read; Linux `Instant` semantics (`CLOCK_MONOTONIC`, excludes suspend) are stable OS behavior; DB comparisons at `leader_renew.sql:3` confirm the authority split. Executed: no test asserts cross-basis equality after a pause (none can, portably). Not executed: hypervisor-pause reproduction.

### m5 `RateLimiter::spawn_background_eviction()` is never called — in-memory limiter’s expired entries accumulate forever by default

- **Where:** `crates/core/src/resilience/rate_limiter.rs:126-135` (helper), `rate_limiter.rs:113-123` (`prune_expired`); no call sites anywhere (`grep -rn spawn_background_eviction` → only its definition and a unit test of `prune_expired` at `tests/integration/resilience.rs:244-263`)
- **What it says:** expiry pruning exists as a public method plus a `tokio::spawn` helper, but no framework code, builder, or doc ever invokes them; expired keys persist in the 16 shards indefinitely.
- **Why it's a problem:** default deployment (app calls `RateLimiter::new`, stacks `RateLimitLayer`) leaks one map entry per unique client key ever seen — the v02 M3/v03 M4 leak, reduced but not closed: sharding fixed contention, not retention. Memory growth on a long-lived public endpoint is unbounded; the fix the operator needs is invisible.
- **Suggested fix:** wire it: `RateLimitLayer::new` (or the builder, Lens 7 territory) spawns `limiter.spawn_background_eviction(Duration::from_secs(30))` when the layer is constructed, or make the limiter lazy-prune every N `check` calls (amortized O(1)) — the latter needs no runtime support and survives `RateLimiter` being cloned across layers. Document the retention bound. Test: pause time, insert 1000 keys, advance past window, assert map size shrinks after interval.
- **Evidence:** Static — grep over the whole crate found only the definition and the unit test; no builder/middleware wiring exists. Executed: `test_rate_limiter_prune_expired_clients` passes (the method works; it is simply never run). Not executed: n/a.

### m6 `InMemoryCacheBackend` tag sets grow monotonically — `delete()` never removes tag members; abandoned tags leak forever

- **Where:** `crates/core/src/cache/backend.rs:54-58, 107-128, 130-136, 155-169`
- **What it says:** `set_with_tags` inserts into `TagMap`; `delete` removes only from `entries`; nothing removes members from tag sets on TTL expiry (checks happen at read time) or on delete; `invalidate_tag` does `tag_map.remove(&tag)` wholesale, which clears one tag’s set — but un-tagged-invalidated tags and explicit deletes leave stale members behind forever.
- **Why it's a problem:** memory grows without bound in “single-instance deployments” (the backend’s stated use) proportionally to total historical tagged writes; `invalidate_tag` later iterates those dead members (`map.remove` returns `None`) — correctness holds, memory and latency degrade silently. It is the in-memory analogue of v03 M2 (which the Redis path remediated via set TTLs), unremediated.
- **Suggested fix:** on `get`/`exists` miss due to expiry, evict the entry *and* scrub it from all tag sets (requires a reverse `key → tags` index, or accept sweep semantics); simpler: in `delete`, iterate `tag_map` and retain only keys != deleted key (O(tags)); plus periodic `retain` of tag sets in `invalidate_tag` and a TTL-aware purge pass mirroring `entries` expiry checks. If scope is refused, document the bound (“tags accumulate for process lifetime; restart to reclaim”).
- **Evidence:** Static — full file read; no code path removes tag members except wholesale `tag_map.remove(&tag)`. Executed: cache integration suite passes (short-lived tests; no long-run memory assertion). Not executed: long-run RSS comparison.

---

## Positive notes

Preserve these — they are the load-bearing parts of the current design:

- **Database-clock authority, uniformly applied.** Every lease/expiry comparison in `lock_*.sql`, `leader_*.sql`, and the prune/get_leader statements uses `clock_timestamp()`; the v03 C1 class is genuinely closed, and the remaining host-clock uses are Redis-side windows only (m1).
- **Monotonic fencing everywhere it counts.** Postgres sequences and Redis `INCR` fence both locks and election terms; tokens/terms are `pub` on `LockGuard`/`LeaderLease` for downstream validation — the correct fencing contract, verified by container suites asserting strictly increasing terms.
- **Hash-tag discipline in Redis Cluster.** Every multi-key script (lock, leader, idempotency) and the quota ZSET are hash-tagged; cache invalidation is single-key `SMEMBERS` + per-key `DEL`. No `CROSSSLOT` path remains; executed suites confirm.
- **Length-prefixed tenant cache keys and tags** (`tenant:{len}:{id}:…`) with delimiter-injection unit tests — the v02 C2 fix is solid and verified.
- **Tenant-scoped rate of failure isolation via length-prefixed idempotency keys** and the compound `(COALESCE(tenant_id,''), idempotency_key)` partial unique index — cross-tenant idempotency collisions are structurally impossible in the Postgres backend.
- **Sorted-set quota design** (`ZREMRANGEBYSCORE` prune → `ZCARD` gate → `ZADD` upsert, all inside one Lua script, with rollback on failed cache write). The architecture is right; only the tag-invalidation attribution seam (M5) undermines it.
- **RFC 9457 error mappings** for tenant/resolver failures (`MISSING_TENANT` 400, `TENANT_NOT_FOUND` 404, `TENANT_CONFLICT` 409) with zero handler boilerplate; `TenantLayer::required` fails closed when no tenant resolves.
- **Subdomain resolver hygiene** — lowercased hosts, bracketed IPv6 parsing, leading-dot base normalization, single-label ASCII validation, and adversarial unit tests (suffix spoofing, multi-level, case) all present and passing.
- **Explicit conflict API** (`try_add_tenant` → `TenantError::Conflict`, HTTP 409) exists — the correct, strict registration shape; it just needs to cover cross-namespace pairs (M7).
- **Redis coordination internals are panic-free by construction** (bounds-checked parsing in `get_leader`; byte-exact length prefixes; no unwraps on untrusted input).
- **Executed assurance:** integration suite **166/166 green**; coordination-relevant container suites **10/10 green** (lock mutual exclusion/fencing, TTL recovery, leader failover, idempotency eviction, quota rejection, tag invalidation). These are real, current results — not claims.
- **`prune_expired()` APIs and `expires_at` indexes** now exist on both coordination backends, using the same DB-clock semantics as the leases themselves.

---

## Verdict — No: the codebase is not ready to proceed / ship for multi-tenant production use.

Zero Critical findings — nothing here silently corrupts data on a normal path. But the lens’s charter questions are answered **no**: tenant isolation is not trustworthy as shipped (M1 dead extension lookup + header-rotation bypass; M5 quota drift; M2/M3 shared-namespace collisions and `FLUSHDB`; M7 silent registration shadowing), cache invalidation is not trustworthy (M4 silently no-ops after short-TTL writes — stale data), and leader-fencing parity across backends is not met (M6). That is 8 Major findings standing against the exact guarantees this lens exists to certify.

**Not ready to ship for multi-tenant deployment or to proceed as a stable base** until the M set lands. All are small, targeted changes (no redesign); a focused remediation pass plus the container-test additions named in each finding would close them. The m-set (m1–m6) is polish.

Charter scorecard (whole-document):

| Charter item | Status |
|---|---|
| 1. Fencing & leader terms (monotonic, DB-clock, renewal-fails-after-expiry, tokens surfaced) | **Met for PostgreSQL** (DB-clock verified); **met for Redis**; **incomplete** — M6 (PG incumbent re-acquire divergence), m4 (host-clock telemetry) |
| 2. Redis Cluster safety (hash tags, Lua atomicity, timeout safety, rate-limiter classification) | **Met** — all multi-key scripts hash-tagged; Lua atomic; `Backend(String)` classification fixed; host-clock windows remain (m1) |
| 3. Tenant isolation (keys, quota, resolver conflicts, cross-tenant leak impossible) | **Not met** — M1, M2, M5, M7; jobs/idempotency/cache namespacing verified sound; **DB pools have no tenant awareness at all** (v03 M5 unremediated) — noted for the conductor, not re-filed |
| 4. Rate limiter (sharding, eviction, tenant-keyed, XFF trust) | **Partially met** — sharded + prunable (v03 M4 substantially remediated); tenant keying broken (M1); background eviction never wired (m5); XFF unvalidated (Lens 5 overlap) |
| 5. Lock/leader operational hygiene (prune, expiration visibility, cleanup) | **Met with gaps** — `prune_expired` + indexes present (manual only); telemetry present but host-derived (m4); TTL-blind in-memory paths (m2, m6) |

Priority order for the fix pass: **M2 → M3** (reserved cache namespace; blast-radius removal), **M4** (stop the stale-read correctness hole), **M1** (restore tenant-keyed limiting / close bypass), **M5** (quota integrity), **M8** (stampede hang), **M6** (election parity), **M7** (registration integrity). Each finding names its acceptance test.

**Verification performed (executed):**
- `cargo test -p rume --all-features --test integration` → `test result: ok. 166 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`
- `cargo test -p rume --all-features --test containers -- coordination redis::test_redis_leader_election_failover_and_terms redis::test_redis_idempotency_store redis::test_redis_cache_tag_invalidation redis::test_redis_cluster_wide_tenant_quota_coordination` → `test result: ok. 7 passed; 0 failed`
- `cargo test -p rume --all-features --test containers -- postgres::test_postgres_leader postgres::test_postgres_distributed_lock` → `test result: ok. 3 passed; 0 failed`
- Docker daemon available; no test was skipped; no source was modified (read-only review).
**Not executed:** full `--test containers` suite (62 tests — only the 10 coordination/cache/idempotency ones relevant to this lens were run); feature-matrix builds (Lens 10’s charter); skewed-clock and pause reproductions for m1/m4/M8 (require loom/fault injection — named in the fixes); destructive `FLUSHDB` (evident from source; deliberately not triggered). Prior reported breakage (`test_redis_idempotency_store`) is confirmed fixed by execution.
