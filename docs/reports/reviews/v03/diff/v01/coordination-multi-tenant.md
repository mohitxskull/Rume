# Distributed Coordination & Multi-Tenancy Diff Review — v03 (v01)

## Summary

This diff review evaluates the distributed coordination and multi-tenant subsystems in Rume across commits `cf82559..HEAD`, auditing `rume/src/coordination/` (`postgres.rs`, `redis.rs`, `lock.rs`, `leader.rs`), `rume/src/tenant/` (`cache.rs`, `quota.rs`, `resolver.rs`, `error.rs`), `rume/src/resilience/` (`rate_limiter.rs`, `redis.rs`, `error.rs`), SQL queries, and integration/container test suites. Foundational architecture and distributed consensus safeguards have significantly matured: database server timestamps (`clock_timestamp()`) have replaced client wall-clock timestamps in PostgreSQL coordination, Redis Cluster hash tags (`{...}`) prevent `CROSSSLOT` failures across locks and leader electors, tenant cache quotas now utilize Redis Sorted Sets to eliminate permanent lockouts on TTL expiration, candidate IDs feature length-prefixed encoding, and tenant-scoped rate limiting and queue identifiers prevent noisy-neighbor collisions. However, the change **fails** diff review and cannot be accepted as-is: `RedisIdempotencyStore::release` was restricted to `IN_PROGRESS` keys, preventing eviction or replay of completed idempotency records and causing `test_redis_idempotency_store` in the container test suite to fail (Finding M1); and `leader_acquire.sql` preserves election terms when an active leader re-acquires its lease, violating monotonic fencing token guarantees across crash-recovery cycles and burning sequence values on conflict (Finding M2).

---

## Prior decisions

- **v03 Coordination C1 (Split-brain leadership and lock theft via client wall-clock skew in PostgreSQL coordination)** — **RESOLVED**.
  `current_epoch_millis()` was completely excised from `rume/src/coordination/postgres.rs`. All lease expiration calculations and evaluations have been moved directly inside PostgreSQL using authoritative server-side time:
  - In `lock_acquire.sql:2, 7` and `leader_acquire.sql:2, 10`, expirations are computed as `(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3` and compared via `<= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT`.
  - In `lock_renew.sql:2, 3` and `leader_renew.sql:2, 3`, lease renewals evaluate `expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT`.
  - In `PostgresLeaderElector::get_leader` (`postgres.rs:255`), active leader queries evaluate against `clock_timestamp()`.
  Client nodes pass only relative TTL durations (`ttl.as_millis().max(1) as i64`), completely eliminating clock drift, virtualization pause, and NTP step vulnerabilities across application worker nodes. Verified in `rume/tests/containers/postgres.rs:469–577`.

- **v03 Coordination C2 (Stale leader re-acquisition fails to increment election term, violating monotonic fencing)** — **CHALLENGED (Defective Remediation)**.
  `rume/src/persistence/sql/postgres/leader_acquire.sql:6–9` was updated to:
  ```sql
  term = CASE
      WHEN rume_leaders.expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
           AND rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term
      ELSE nextval('rume_leader_term_seq')
  END
  ```
  When a lease has expired (`expires_at <= clock_timestamp()`), any new acquisition—even by the same candidate—now correctly evaluates the `ELSE` branch and advances `term`. However, the remediation is defective: the query retains `OR rume_leaders.candidate_id = $2` in the `WHERE` clause, permitting an active leader to re-acquire leadership with the **exact same term** without incrementing the sequence. If a node crashes, loses in-memory task state, and restarts before its lease TTL elapses, it re-acquires leadership with the old term, defeating split-brain fencing for downstream consumers and diverging from `RedisLeaderElector`. Re-reported as Finding M2.

- **v03 Coordination C3 (Redis coordination and cache Lua scripts break under Redis Cluster due to missing hash tags (`CROSSSLOT`))** — **RESOLVED**.
  In `rume/src/coordination/redis.rs:58, 62, 240, 244`, resource names and election roles are now wrapped in `{...}` Redis hash tags:
  - `RedisDistributedLock`: `lock_key = format!("{}{{{}}}", self.prefix, resource)`, `fence_key = format!("{}fence:{{{}}}", self.prefix, resource)`.
  - `RedisLeaderElector`: `leader_key = format!("{}{{{}}}", self.prefix, role)`, `term_key = format!("{}term:{{{}}}", self.prefix, role)`.
  In `rume/src/tenant/quota.rs:93`, quota keys are hash-tagged (`format!("{}quota:{{{}}}:keys", self.prefix, tenant_id)`).
  In `rume/src/cache/redis.rs:224–233`, the multi-key `DEL` command that previously passed arbitrary cached keys alongside the tag set key was replaced with single-key `DEL` executions for each key followed by `DEL` on the tag set itself, completely eliminating `CROSSSLOT Keys in request don't hash to the same slot` errors in clustered topologies.

- **v03 Coordination C4 (Cross-tenant job and idempotency key collisions permit cross-tenant rejection and result leakage)** — **CHALLENGED (Defective Remediation)**.
  Tenant isolation for background jobs and idempotency keys was implemented across storage engines:
  - `rume_jobs` in `rume/src/persistence/sql/postgres/jobs_schema.sql:12, 26` added a `tenant_id TEXT` column and updated the partial unique index to `(COALESCE(tenant_id, ''), idempotency_key) WHERE idempotency_key IS NOT NULL AND status NOT IN ('dead_letter', 'purged')`.
  - `Job::tenant_id(&self) -> Option<String>` was added to `rume/src/jobs/job.rs:38`.
  - `scoped_idempotency_key` in `rume/src/jobs/reliability.rs:37` formats tenant keys with explicit length prefixing: `format!("tenant:{}:{}:idemp:{key}", tenant_id.len(), tenant_id)`.
  - `JobWorker::execute_job` (`rume/src/jobs/worker.rs:324–330`) computes `effective_key` using length-prefixed tenant qualification for all idempotency store operations.
  However, the remediation in `rume/src/jobs/redis.rs:320–334` restricted `RedisIdempotencyStore::release` to keys with value `'IN_PROGRESS'`, making it impossible to evict or release `'COMPLETED'` records and causing `test_redis_idempotency_store` in `rume/tests/containers/redis.rs:534` to panic and fail. Reported as Finding M1.

- **v03 Coordination M1 (`TenantScopedCache` quota leak on TTL expiration permanently locks out tenant caching)** — **RESOLVED**.
  `RedisTenantQuotaStore` in `rume/src/tenant/quota.rs:109–162` was rewritten to maintain active keys in a Redis Sorted Set scored by millisecond expiration timestamp (`expires_at_ms`).
  In `record_key()`, an atomic Lua script prunes expired members via `ZREMRANGEBYSCORE KEYS[1] '-inf' now_ms` before evaluating active key count via `ZCARD KEYS[1]`. If the key already exists (`ZSCORE`), updates are permitted even at quota capacity.
  In `TenantScopedCache::set_with_tags` (`rume/src/tenant/cache.rs:103–128`), `record_key` passes the key's TTL, and if the subsequent `set_json_with_tags` fails, `quota.remove_key(tenant_id, key)` is invoked immediately in a rollback block.
  Passive TTL expiration in Redis no longer leaks quota permits, permanently resolving tenant caching lockouts. Verified in `rume/tests/integration/tenant.rs:330–389`.

- **v03 Coordination M2 (Monotonic memory leak in Redis tag invalidation sets from expired TTL keys)** — **RESOLVED**.
  In `rume/src/cache/redis.rs:156–163`, whenever `set_with_tags` writes a key with a TTL, it executes `PEXPIRE` on each associated tag set key `tk`, setting its lifetime to the key's TTL plus an auxiliary 1-hour buffer (`tag_ttl_ms = (ttl.as_millis() as u64).saturating_add(3_600_000)`). If no active keys refresh the tag set, Redis automatically evicts the tag set, preventing unbounded RAM growth. Verified in `rume/tests/containers/redis.rs:242–280`.

- **v03 Coordination M3 (Delimiter collision in `RedisLeaderElector` candidate IDs breaks leader recovery)** — **RESOLVED**.
  In `rume/src/coordination/redis.rs:274`, candidate IDs stored in Redis are encoded with explicit length prefixing:
  ```lua
  local val = tostring(string.len(ARGV[1])) .. ':' .. ARGV[1] .. ':' .. tostring(term)
  ```
  In `renew` and `resign` (lines 327, 411), prefix comparisons include the length-prefix delimiter (`tostring(string.len(ARGV[1])) .. ':' .. ARGV[1] .. ':'`), preventing prefix collisions between `"worker"` and `"worker:node"`.
  In `get_leader` (lines 376–387), `s.split_once(':')` parses the length prefix first, verifies `&rest[len..=len] == ":"`, and extracts the exact candidate ID substring regardless of how many colons it contains. Verified in `rume/tests/integration/coordination.rs:92–143`.

- **v03 Coordination M4 (Global lock contention and O(N) eviction loop in `RateLimiter::check`)** — **RESOLVED (Partially)**.
  The catastrophic $O(N)$ inline eviction loop (`if clients.len() > 256 { clients.retain(...) }`) was deleted from `rume/src/resilience/rate_limiter.rs:66–96`. `RateLimiter::check` now operates strictly in $O(\text{timestamps})$ for the targeted client key.
  `prune_expired(&self)` was extracted to an explicit method (lines 99–107). (Note: `RateLimiter` still uses a global `RwLock<BTreeMap>` and lacks background reaper scheduling, tracked as minor finding m3).

- **v03 Coordination M5 (Complete lack of multi-tenant resource isolation in rate limiting and database pools)** — **RESOLVED (Partially)**.
  In `rume/src/resilience/rate_limiter.rs:160–174`, `RateLimitService::call` now inspects the `x-tenant-id` header first, formatting the rate limiting key as `"tenant:{tenant_id}"`. When absent, it parses `X-Forwarded-For` by extracting and trimming the first comma-separated client IP (`"ip:{ip}"`), preventing noisy-neighbor IP exhaustion across distinct tenants. (Note: Bypassing `TenantContext` extensions is tracked in finding m2).

- **v03 Coordination M6 (`HeaderTenantResolver::add_tenant` silently overwrites conflicting slugs and precedence shadows vanity slugs)** — **RESOLVED (Partially)**.
  `HeaderTenantResolver::try_add_tenant` was introduced in `rume/src/tenant/resolver.rs:54–83`, returning `Err(TenantError::Conflict(...))` if `tenant.slug` is already registered under a different tenant ID, or if `tenant.id` is already registered with a different slug. `add_tenant` delegates to `try_add_tenant` and logs warnings on conflicts. `TenantError::Conflict` maps to HTTP 409 `TENANT_CONFLICT`. Verified in `rume/tests/integration/tenant.rs:173–185`. (Note: ID-over-slug resolution precedence remains deliberate and documented in test lines 126–170).

- **v03 Coordination m1 (Subdomain tenant resolver fails on case-insensitive hosts and IPv6 addresses)** — **RESOLVED**.
  In `rume/src/tenant/resolver.rs:182–193`, `SubdomainTenantResolver` normalizes host headers using `host_str.to_ascii_lowercase()` per RFC 7230 §5.4. For IPv6 host literals starting with `'['`, it extracts the bracketed address (`&host_lower[..=bracket_end]`) rather than naively splitting on colons, preserving port stripping without breaking IPv6 parsing. Verified by `test_subdomain_tenant_resolver_case_insensitivity` in `rume/tests/integration/tenant.rs:188–214`.

- **v03 Coordination m2 (`RedisRateLimiter` misclassifies Redis connection and script errors as `CircuitOpen`)** — **RESOLVED**.
  `ResilienceError::Backend(String)` was added to `rume/src/resilience/error.rs:27`. In `rume/src/resilience/redis.rs:60, 104`, deadpool pool acquisition errors and Lua script execution failures are mapped to `ResilienceError::Backend(e.to_string())`, which converts to `AppError::internal` (HTTP 500) rather than falsely reporting `CIRCUIT_BREAKER_OPEN` (HTTP 503).

- **v03 Coordination m3 (`LockGuard` and `LeaderLease` lack expiration timestamp telemetry)** — **RESOLVED**.
  `LockGuard` (`rume/src/coordination/lock.rs:24–55`) and `LeaderLease` (`rume/src/coordination/leader.rs:18–49`) now feature `pub expires_at: Option<std::time::Instant>`, constructor `with_expires_at()`, and helper inspection methods `is_expired() -> bool` and `time_remaining() -> Option<Duration>`. All coordination implementations in PostgreSQL and Redis populate `expires_at = Some(Instant::now() + ttl)`.

- **v03 Coordination m4 (Unused global sequence burn in `PostgresDistributedLock::acquire` conflict path)** — **RESOLVED**.
  In `rume/src/persistence/sql/postgres/lock_acquire.sql:5`, the `ON CONFLICT (resource) DO UPDATE` clause now sets `fencing_token = EXCLUDED.fencing_token` instead of invoking `nextval('rume_lock_fencing_seq')` a second time. Exactly one sequence value is evaluated from the `VALUES` clause per conflict update.

- **v03 Coordination m5 (Expired rows in `rume_locks` and `rume_leaders` are retained indefinitely until next access)** — **RESOLVED**.
  Partial indexes on `expires_at` were added in `locks_schema.sql:7` (`CREATE INDEX IF NOT EXISTS idx_rume_locks_expires_at ON rume_locks (expires_at)`) and `leader_schema.sql:7` (`CREATE INDEX IF NOT EXISTS idx_rume_leaders_expires_at ON rume_leaders (expires_at)`).
  In addition, `prune_expired(&self) -> Result<usize, CoordinationError>` was implemented on both `PostgresDistributedLock` (`postgres.rs:59–65`) and `PostgresLeaderElector` (`postgres.rs:176–182`), executing indexed `DELETE ... WHERE expires_at <= clock_timestamp()`.

---

## Findings

### M1 `RedisIdempotencyStore::release` ignores `COMPLETED` records, preventing key eviction and breaking container test suite

- **Where:** `rume/src/jobs/redis.rs:320–334`, `rume/tests/containers/redis.rs:528–535`
- **What it says:**
  In `rume/src/jobs/redis.rs`:
  ```rust
  // Only delete the lock if it is still IN_PROGRESS, preventing deletion of completed jobs or peer claims
  let script = redis::Script::new(
      r#"
      local current = redis.call('GET', KEYS[1])
      if current == 'IN_PROGRESS' then
          redis.call('DEL', KEYS[1])
          return 1
      else
          return 0
      end
      "#,
  );

  let _: i64 = script
      .key(&redis_key)
      .invoke_async(&mut *conn)
      .await
      .map_err(|e| JobError::Fatal(e.to_string()))?;
  ```
- **Why it's a problem:**
  The `IdempotencyStore::release(&self, key: &str)` trait method is the sole programmatic mechanism for evicting or releasing an idempotency key.
  When a job completes successfully and records its result via `complete(key, payload)`, the key's value in Redis is set to `'COMPLETED:<hex>'` with a 24-hour TTL.
  Under the current script, if an administrative routine, test reset, or saga compensator subsequently invokes `release(key)`, the script checks `if current == 'IN_PROGRESS'`. Because the key contains `'COMPLETED:...'`, the script takes the `else` branch and returns `0` without executing `DEL`. The completed record remains cached in Redis until its 24-hour TTL elapses. Any subsequent `try_acquire(key)` call continues returning `IdempotencyStatus::Completed(Some(...))` rather than `IdempotencyStatus::New`.
  This defect breaks the standard test suite. Running `cargo test --all-features --test containers -- redis::test_redis_idempotency_store` panics deterministically:
  ```
  thread 'redis::test_redis_idempotency_store' (895208) panicked at rume/tests/containers/redis.rs:534:5:
  assertion `left == right` failed
    left: Completed(Some([111, 114, 100, 101, 114, 45, 105, 100, 58, 52, 53, 54]))
   right: New
  ```
  Line 529 of the test calls `store.release("tx-uuid-999")` expecting the key to be cleared so the subsequent `try_acquire` returns `New`. Because `release` silently ignores completed keys, the test fails.
- **Suggested fix:**
  Allow `release()` to clear both `'IN_PROGRESS'` and `'COMPLETED'` records for the given key, or execute an unconditional `DEL` (or provide an explicit `delete()` / `reset()` method if differentiating between lock release and cache eviction):
  ```lua
  local current = redis.call('GET', KEYS[1])
  if current == 'IN_PROGRESS' or string.sub(current, 1, 10) == 'COMPLETED:' then
      redis.call('DEL', KEYS[1])
      return 1
  else
      return 0
  end
  ```

---

### M2 `leader_acquire.sql` preserves election term on unexpired re-acquisition, defeating monotonic fencing on node crash recovery

- **Where:** `rume/src/persistence/sql/postgres/leader_acquire.sql:6–12`, `rume/src/coordination/postgres.rs:188–215`
- **What it says:**
  ```sql
  INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
  VALUES ($1, $2, nextval('rume_leader_term_seq'), (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3)
  ON CONFLICT (role) DO UPDATE
  SET candidate_id = EXCLUDED.candidate_id,
      term = CASE
          WHEN rume_leaders.expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
               AND rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term
          ELSE nextval('rume_leader_term_seq')
      END,
      expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3
  WHERE rume_leaders.expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
     OR rume_leaders.candidate_id = $2
  RETURNING term;
  ```
- **Why it's a problem:**
  In distributed consensus, `LeaderElector` provides two fundamentally different operations:
  1. `try_acquire()` — An election attempt to claim leadership. Every successful grant must increment the monotonic election term/fencing token.
  2. `renew()` — Heartbeat lease extension of an already-held active lease, which preserves the term (implemented via `leader_renew.sql`).

  By including `OR rume_leaders.candidate_id = $2` in the `WHERE` clause of `leader_acquire.sql` and preserving `term` when `rume_leaders.expires_at > clock_timestamp()`, `PostgresLeaderElector::try_acquire` acts as an implicit re-entrant renewal that bypasses `renew()`.
  
  This creates a split-brain fencing vulnerability across node crashes:
  1. Node 1 acquires leadership for `"cluster-scheduler"` with a 30-second TTL at term 5.
  2. At $T = 3\text{s}$, Node 1 crashes (OOM-killed or panics). All in-memory state and locks on Node 1 are lost.
  3. At $T = 6\text{s}$, Node 1 restarts. On application startup, Node 1 calls `try_acquire("cluster-scheduler", "node-1", 30s)`.
  4. Because `rume_leaders.candidate_id = 'node-1'` and `expires_at > clock_timestamp()`, `leader_acquire.sql` grants leadership with the **exact same term 5**.
  5. Downstream storage systems and event relays checking `LeaderLease.term` cannot distinguish between pre-crash operations and post-crash operations. Stale in-flight mutations from the crashed instance are accepted alongside new writes.
  
  Furthermore, this violates behavioural consistency with `RedisLeaderElector`: in `RedisLeaderElector::try_acquire` (`rume/src/coordination/redis.rs:269–285`), if a lease is active (even for the same candidate), `try_acquire` returns `None`, and whenever an acquisition succeeds, it unconditionally increments the term via `INCR KEYS[2]`.
- **Suggested fix:**
  Remove `OR rume_leaders.candidate_id = $2` and the `CASE` statement from `leader_acquire.sql`. Require `try_acquire` to succeed only when the previous lease has expired, and advance the term unconditionally (using `EXCLUDED.term` to also prevent sequence burn):
  ```sql
  INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
  VALUES ($1, $2, nextval('rume_leader_term_seq'), (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3)
  ON CONFLICT (role) DO UPDATE
  SET candidate_id = EXCLUDED.candidate_id,
      term = EXCLUDED.term,
      expires_at = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3
  WHERE rume_leaders.expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
  RETURNING term;
  ```
  Lease renewals while active remain cleanly handled by `renew()` (`leader_renew.sql`).

---

### m1 Duplicate sequence evaluation in `leader_acquire.sql` conflict update path burns term sequence numbers

- **Where:** `rume/src/persistence/sql/postgres/leader_acquire.sql:2, 8`
- **What it says:**
  ```sql
  INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
  VALUES ($1, $2, nextval('rume_leader_term_seq'), ...)
  ON CONFLICT (role) DO UPDATE
  SET candidate_id = EXCLUDED.candidate_id,
      term = CASE
          WHEN ... THEN rume_leaders.term
          ELSE nextval('rume_leader_term_seq')
      END,
      ...
  ```
- **Why it's a problem:**
  In PostgreSQL, expressions in the `VALUES` clause are evaluated before conflict detection. When an acquisition conflicts on `role` and the expired lease is re-assigned, `nextval('rume_leader_term_seq')` has already been evaluated for `EXCLUDED.term`.
  The `DO UPDATE` clause then executes `ELSE nextval('rume_leader_term_seq')`, evaluating the sequence a second time. As a result, `rume_leader_term_seq` advances by 2 on every leadership failover rather than 1. (In contrast, `lock_acquire.sql:5` correctly avoided sequence burn by assigning `fencing_token = EXCLUDED.fencing_token`).
- **Suggested fix:**
  In `leader_acquire.sql`, replace `ELSE nextval('rume_leader_term_seq')` with `ELSE EXCLUDED.term`.

---

### m2 `RateLimitService` ignores `TenantContext` extensions, bypassing tenant rate limiting for subdomain-routed requests

- **Where:** `rume/src/resilience/rate_limiter.rs:160–174`
- **What it says:**
  ```rust
  let tenant_key = req
      .headers()
      .get("x-tenant-id")
      .and_then(|h| h.to_str().ok())
      .map(|t| format!("tenant:{}", t.trim()));

  let client_key = tenant_key.unwrap_or_else(|| {
      req.headers()
          .get("x-forwarded-for")
          .and_then(|h| h.to_str().ok())
          .and_then(|s| s.split(',').next())
          .map(|ip| format!("ip:{}", ip.trim()))
          .unwrap_or_else(|| "client_default".to_string())
  });
  ```
- **Why it's a problem:**
  `TenantLayer` resolves tenants across multiple strategies (including `Host` header inspection in `SubdomainTenantResolver`) and inserts the resolved `Tenant` into request extensions (`req.extensions_mut().insert(tenant)`).
  However, `RateLimitService::call` extracts the tenant key exclusively by inspecting the literal `x-tenant-id` HTTP header.
  When an application routes traffic using vanity subdomains (e.g. `acme.myplatform.com`) and stacks `RateLimitLayer` inside `TenantLayer`, requests carry no `x-tenant-id` header. `RateLimitService` fails to detect the tenant identity, falling back to IP extraction from `X-Forwarded-For`. All multi-tenant rate limiting and per-tenant burst protections are bypassed for subdomain-based traffic.
- **Suggested fix:**
  In `RateLimitService::call`, inspect `req.extensions().get::<Tenant>()` (or `TenantContext`) before inspecting headers:
  ```rust
  let tenant_key = req
      .extensions()
      .get::<crate::tenant::Tenant>()
      .map(|t| format!("tenant:{}", t.id))
      .or_else(|| {
          req.headers()
              .get("x-tenant-id")
              .and_then(|h| h.to_str().ok())
              .map(|t| format!("tenant:{}", t.trim()))
      });
  ```

---

### m3 `RateLimiter` retains global write lock serialization and lacks background eviction scheduling

- **Where:** `rume/src/resilience/rate_limiter.rs:66–96, 99–107`
- **What it says:**
  ```rust
  pub fn check(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
      let mut clients = self.clients.write().unwrap();
      ...
  ```
- **Why it's a problem:**
  While the $O(N)$ inline retention loop across all clients was removed from `check()`, `RateLimiter` still acquires an exclusive write lock on `Arc<RwLock<BTreeMap<String, ClientEntry>>>` on every incoming HTTP request. Under multicore high-concurrency benchmarks (e.g. 50,000 req/sec), all worker threads serialize on this single mutex.
  Furthermore, `prune_expired(&self)` is only available as an uncalled public method; without a background reaper task, memory for inactive client entries accumulates monotonically until application restart.
- **Suggested fix:**
  Replace `Arc<RwLock<BTreeMap>>` with a lock-striped concurrent map (e.g. `dashmap::DashMap`) and schedule a background task (e.g. via `tokio::spawn` every 30 seconds) to invoke `prune_expired()`.

---

## Positive notes

- **Authoritative server-side coordination timestamps**: Eliminating `current_epoch_millis()` in favor of PostgreSQL `clock_timestamp()` closes the split-brain and premature lease expiration vulnerabilities caused by client clock drift and virtualization pauses.
- **Redis Cluster `{...}` hash tag isolation**: Systematic tagging of resource identifiers (`lock:{{{}}}` and `leader:{{{}}}`) guarantees identical hash slot mapping across distributed locks, fencing counters, and election terms, eliminating `CROSSSLOT` runtime crashes in clustered Redis topologies.
- **Sorted Set tenant quota enforcement with automatic rollback**: Transitioning `RedisTenantQuotaStore` to Redis Sorted Sets with `ZREMRANGEBYSCORE` and `ZCARD` guarantees that keys expiring via TTL release their quota permits immediately, preventing tenant lockouts while ensuring failed cache writes roll back acquired permits.
- **Length-prefixed candidate ID encoding**: `RedisLeaderElector` candidate encoding (`"{len}:{id}:{term}"`) prevents delimiter collision bugs and route hijacking when IDs contain colons or shared prefix strings.
- **Strict registration conflict detection**: `HeaderTenantResolver::try_add_tenant` returns explicit `TenantError::Conflict` (HTTP 409) on duplicate slug or ID registrations, preventing silent tenant route overwrites.
- **Robust RFC-compliant host normalization**: `SubdomainTenantResolver` properly lowercases HTTP Host headers and parses bracketed IPv6 literals, preventing case-sensitive 404s and port-splitting failures.
- **Lease expiration telemetry**: Monotonic timestamps and helper methods (`is_expired()`, `time_remaining()`) on `LockGuard` and `LeaderLease` provide essential observability for long-running worker loops.
- **Indexed table maintenance**: Indexes on `expires_at` and `prune_expired()` APIs on PostgreSQL coordination backends prevent unbounded table bloat.

---

## Verdict

**Fail** — this change cannot be accepted until a fix pass lands.

While the distributed coordination and multi-tenant subsystems have resolved 11 of the 15 prior findings (including Critical findings C1 and C3, and Major findings M1, M2, and M3), two actionable defects prevent production acceptance:
1. **Finding M1** breaks the container test suite (`cargo test --all-features --test containers -- redis::test_redis_idempotency_store`), because `RedisIdempotencyStore::release` silently ignores `'COMPLETED'` records, preventing idempotency key eviction and test reset.
2. **Finding M2** leaves a split-brain fencing loophole in `leader_acquire.sql`, allowing crashed nodes restarting within their TTL to re-acquire leadership with unchanged election terms.

A remediation pass addressing **Findings M1 and M2** must land before merging this diff.
