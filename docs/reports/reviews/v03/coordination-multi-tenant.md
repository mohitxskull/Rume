# Distributed Coordination & Multi-Tenancy Review — v03

## Summary

The Rume distributed coordination and multi-tenancy subsystems provide foundational building blocks for enterprise multi-node deployments: PostgreSQL-backed and Redis-backed distributed locks with fencing tokens, leader election with lease terms, tenant context resolution with length-prefixed cache isolation, and sliding-window rate limiters. However, the subsystem is **not ready for production** due to severe distributed consensus and cross-tenant isolation vulnerabilities: client wall-clock reliance in PostgreSQL coordination allows clock-skewed nodes to steal locks and leadership before leases expire; stale leaders re-acquiring expired leases retain previous term numbers, defeating fencing against split-brain mutations; Redis coordination Lua scripts omit hash tags (`{...}`), causing immediate `CROSSSLOT` failures on Redis Cluster; and shared job queue and idempotency stores lack tenant isolation, allowing cross-tenant idempotency collisions and job payload leaks. Additionally, tenant cache quotas permanently lock out caching when keys expire via TTL, Redis tag sets leak dead keys indefinitely, and candidate ID colons break leader recovery.

---

## Prior decisions

In accordance with `.opencode/skills/review-authoring/SKILL.md`, prior findings and architectural decisions from `docs/reports/reviews/v02/` intersecting with distributed coordination, multi-tenancy, cache isolation, and rate limiting have been audited against the codebase:

- **v02 Security C2 (Multi-tenant cache key delimiter collision permits cross-tenant cache poisoning)** — **REAFFIRMED (Partial / Needs hardening)**. Verified in `crates/core/src/tenant/cache.rs:15–22`. `scoped_cache_key` and `tenant_cache_tag` now format keys and tags using explicit length prefixing (`format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)`), successfully preventing colon-delimiter collision attacks across tenant IDs and keys. However, new vulnerabilities were introduced in v03: `RedisTenantQuotaStore` never decrements quota counters when cached keys expire via TTL (finding M1), and `RedisCacheBackend` leaks dead keys in Redis tag sets indefinitely (finding M2).
- **v02 Security M1 (Subdomain tenant resolver permits suffix-matching domain spoofing)** — **REAFFIRMED**. Verified in `crates/core/src/tenant/resolver.rs:158–176` and `crates/core/tests/tenant_suite.rs:104–121`. The resolver normalizes `base_domain` with a leading dot (`.{base_domain}`) and verifies that extracted subdomains contain no interior dots (`!subdomain.contains('.')`) and strictly adhere to ASCII alphanumeric/hyphen syntax, successfully rejecting suffix-spoofing and multi-level subdomain hijacking. (Note minor case-sensitivity and IPv6 parsing gap in finding m1).
- **v02 Security M2 (`HeaderTenantResolver` shares a single map for IDs and slugs)** — **REAFFIRMED (Partial)**. Verified in `crates/core/src/tenant/resolver.rs:30–62, 85–109`. Tenant IDs and vanity slugs are now stored in separate maps (`by_id` and `by_slug`), and `resolve()` queries `by_id` first before falling back to `by_slug`. However, `add_tenant` silently overwrites duplicate slug registrations without conflict checks, and ID-over-slug lookup precedence can shadow tenant vanity slugs without warning (finding M6).
- **v02 Enterprise-Reliability M6 (`LeaderElector` lacks monotonic fencing tokens or epoch tracking)** — **CHALLENGED (Defective Remediation)**. While `LeaderLease` now carries a `term: u64` across `PostgresLeaderElector` and `RedisLeaderElector`, the remediation is defective:
  1. In `PostgresLeaderElector` (`leader_acquire.sql`), if a leader's lease expires and that same leader re-acquires the lease, the term is NOT incremented, allowing a recovered leader to execute conflicting mutations with the old term (finding C2).
  2. Both `PostgresDistributedLock` and `PostgresLeaderElector` rely on client `SystemTime::now()` rather than database server time, permitting clock-skewed nodes to steal locks and leadership before leases expire (finding C1).
  3. In `RedisLeaderElector`, multi-key Lua scripts lack Redis hash tags (`{role}`), causing immediate `CROSSSLOT` failures on Redis Cluster (finding C3).
  4. Candidate IDs and terms are concatenated with an unescaped colon delimiter (`candidate_id:term`), causing `get_leader` to fail when IDs contain colons (finding M3).
- **v02 Enterprise-Reliability M3 (`RateLimiter` permanently leaks memory for inactive clients)** — **CHALLENGED (Defective Remediation)**. While passive eviction when `clients.len() > 256` and `prune_expired()` were introduced in `crates/core/src/resilience/rate_limiter.rs:92–115`, the remediation introduces critical performance defects: the check performs an $O(N)$ full-scan retention loop inside an exclusive write lock on every single request once client count exceeds 256 (finding M4). Furthermore, client identification remains hardcoded to unvalidated `x-forwarded-for` with no tenant isolation support (finding M5), and `RedisRateLimiter` misclassifies connection failures as `CircuitOpen` (finding m2).
- **v02 Enterprise-Reliability m4 (`TenantScopedCache` and `Cache` omit tag prefix namespacing and tenant memory quotas)** — **CHALLENGED (Defective Remediation)**. While tags are now scoped via `scoped_tag` (`tenant:{len}:{id}:tag:{tag}`) and `TenantQuotaStore` / `RedisTenantQuotaStore` was introduced, quota permits are never released on TTL expiration, permanently locking out tenants from caching data once the quota ceiling is hit (finding M1). Additionally, `invalidate_tag` multi-key `DEL` fails on Redis Cluster (finding C3).
- **Production PostgreSQL & Redis Coordination Backends (`PostgresDistributedLock`, `RedisDistributedLock`, `PostgresLeaderElector`, `RedisLeaderElector`)** — **NOTE-NEW**. New production distributed coordination implementations introduced in Phase 5 / SQLx modernization; audited in findings C1, C2, C3, M3, m3, m4, and m5.
- **Tenant Context Propagation & Extraction (`TenantContext`, `OptionalTenant`, `TenantLayer`)** — **NOTE-NEW**. Axum 0.8 / Tower tenant extraction middleware audited in finding M5 and Positive Notes.

---

## Findings

### C1 Split-brain leadership and lock theft via client wall-clock skew in PostgreSQL coordination

- **Where:** `crates/core/src/coordination/postgres.rs:14–19, 74–85, 102–113, 186–198, 220–232, 248–257`, `crates/core/src/persistence/sql/postgres/lock_acquire.sql:7`, `crates/core/src/persistence/sql/postgres/leader_acquire.sql:10`, `crates/core/src/persistence/sql/postgres/leader_renew.sql:3`
- **What it says:**
  In `crates/core/src/coordination/postgres.rs`:
  ```rust
  fn current_epoch_millis() -> i64 {
      std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or_default()
          .as_millis() as i64
  }
  ```
  In `acquire()` and `try_acquire()`:
  ```rust
  let now = current_epoch_millis();
  let expires_at = now.saturating_add(ttl.as_millis() as i64);

  let row = sqlx::query(LEADER_ACQUIRE_SQL)
      .bind(&r)
      .bind(&cid)
      .bind(expires_at)
      .bind(now)
      .fetch_optional(&self.pool)
      .await...
  ```
  In `leader_acquire.sql`:
  ```sql
  WHERE rume_leaders.expires_at <= $4 OR rume_leaders.candidate_id = $2
  ```
- **Why it's a problem:** In distributed systems, relying on client-side wall-clock timestamps (`SystemTime::now()`) across distinct application machines for lease expiration comparisons is fundamentally unsound due to NTP drift, clock skew, and virtualization pauses.
  1. **Premature lock/leadership theft:** Suppose Node A holds an active leadership lease with TTL = 10s until database timestamp $T + 10\,000$. If Node B's clock drifts ahead by 5 seconds (or during an NTP step), Node B evaluates `try_acquire` with `$4 = T + 11\,000$. The database condition `rume_leaders.expires_at <= $4` evaluates to `true` on Node B, even though Node A has 4 seconds remaining on its active lease.
  2. **Simultaneous active leaders (Split-Brain):** Node A believes it is the legitimate leader and continues issuing mutations, while Node B believes it has successfully acquired leadership. Both nodes execute leader-only routines simultaneously, corrupting persistent state and violating mutual exclusion.
  3. **Premature renewal rejection:** Conversely, if Node A's clock drifts backwards, `expires_at > $4` in `leader_renew.sql` or `lock_renew.sql` can evaluate to false, causing legitimate renewals to fail prematurely with `LockExpired`.
  4. PostgreSQL has an authoritative, monotonically increasing database clock: `(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT`. Relying on client-supplied timestamps for server-side lease validation defeats distributed coordination guarantees.
- **Suggested fix:** Eliminate client-supplied `$now` timestamps in coordination queries:
  1. Pass only duration `$ttl_ms` to PostgreSQL.
  2. In `lock_acquire.sql`, `lock_renew.sql`, `leader_acquire.sql`, and `leader_renew.sql`, compute expiration and evaluate comparisons directly using database server time:
     ```sql
     -- Example in leader_acquire.sql:
     INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
     VALUES ($1, $2, nextval('rume_leader_term_seq'), (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT + $3)
     ON CONFLICT (role) DO UPDATE
     SET candidate_id = EXCLUDED.candidate_id,
         term = CASE
             WHEN rume_leaders.expires_at > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
                  AND rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term
             ELSE nextval('rume_leader_term_seq')
         END,
         expires_at = EXCLUDED.expires_at
     WHERE rume_leaders.expires_at <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
        OR rume_leaders.candidate_id = $2
     RETURNING term;
     ```

---

### C2 Stale leader re-acquisition fails to increment election term, violating monotonic fencing

- **Where:** `crates/core/src/persistence/sql/postgres/leader_acquire.sql:3–10`
- **What it says:**
  ```sql
  INSERT INTO rume_leaders (role, candidate_id, term, expires_at)
  VALUES ($1, $2, nextval('rume_leader_term_seq'), $3)
  ON CONFLICT (role) DO UPDATE
  SET candidate_id = EXCLUDED.candidate_id,
      term = CASE
          WHEN rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term
          ELSE nextval('rume_leader_term_seq')
      END,
      expires_at = EXCLUDED.expires_at
  WHERE rume_leaders.expires_at <= $4 OR rume_leaders.candidate_id = $2
  RETURNING term;
  ```
- **Why it's a problem:** In distributed consensus and active-passive failover, every transition from an expired lease into a newly granted lease **must** increment the election term/epoch. This guarantees that downstream storage engines, outbox relays, and worker queues can fence out lingering mutations initiated prior to lease expiration.
  1. When Candidate A holds leadership for role `"scheduler"`, its lease expires at time $T$. Candidate A pauses (due to GC, network partition, or process crash).
  2. While the lease is expired, no other candidate claims the role.
  3. Candidate A wakes up or restarts, and calls `try_acquire("scheduler", "candidate-a", ttl)`.
  4. The conflict update checks `CASE WHEN rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term ELSE nextval(...) END`. Because the candidate ID matches (`"candidate-a" == "candidate-a"`), `term` is **not incremented** and retains the previous term value.
  5. Candidate A acquires the lease with the exact same term it held before its lease expired and before it crashed.
  6. Downstream storage systems and event relays checking `LeaderLease.term` cannot distinguish between: (a) continuous uninterrupted leadership, and (b) a crash/pause where in-memory state was wiped and lease ownership lapsed. Stale in-flight writes from before the crash are accepted alongside new writes, defeating split-brain fencing.
- **Suggested fix:** Term preservation must require **both** matching candidate ID and an unexpired lease. If the lease expired (`expires_at <= $now`), any new acquisition—even by the same candidate—must increment the term:
  ```sql
  term = CASE
      WHEN rume_leaders.expires_at > $4 AND rume_leaders.candidate_id = EXCLUDED.candidate_id THEN rume_leaders.term
      ELSE nextval('rume_leader_term_seq')
  END
  ```

---

### C3 Redis coordination and cache Lua scripts break under Redis Cluster due to missing hash tags (`CROSSSLOT`)

- **Where:** `crates/core/src/coordination/redis.rs:87–107, 268–289`, `crates/core/src/cache/redis.rs:215–224`
- **What it says:**
  In `RedisDistributedLock::acquire`:
  ```rust
  let lock_key = self.lock_key(&res);   // "rume:lock:resource"
  let fence_key = self.fence_key(&res); // "rume:lock:fence:resource"
  ...
  let script = redis::Script::new(r#"
      local acquired = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'PX', ARGV[2])
      if acquired then
          local fence = redis.call('INCR', KEYS[2])
          return fence
      ...
  "#);
  ```
  In `RedisLeaderElector::try_acquire`:
  ```rust
  let leader_key = self.leader_key(&r); // "rume:leader:role"
  let term_key = self.term_key(&r);     // "rume:leader:term:role"
  ...
  let script = redis::Script::new(r#"
      local current = redis.call('GET', KEYS[1])
      if not current then
          local term = redis.call('INCR', KEYS[2])
          ...
  "#);
  ```
  In `RedisCacheBackend::invalidate_tag`:
  ```rust
  let mut del_cmd = redis::cmd("DEL");
  for k in &keys {
      del_cmd.arg(k);
  }
  del_cmd.arg(&tk);
  del_cmd.query_async::<()>(&mut *conn).await...
  ```
- **Why it's a problem:** In Redis Cluster, the keyspace is sharded into 16,384 hash slots based on `CRC16(key) % 16384`. Redis strictly forbids multi-key commands and multi-key Lua scripts (`KEYS[1]`, `KEYS[2]`) unless all keys belong to the **exact same hash slot**.
  1. `RedisDistributedLock::acquire` accesses `KEYS[1]` (`rume:lock:<res>`) and `KEYS[2]` (`rume:lock:fence:<res>`). Because neither key contains a Redis hash tag `{...}`, Redis hashes the entire string. In ~99.99% of cases, the lock key and fence key hash to different slots. When executed against Redis Cluster, Redis immediately aborts with: `CROSSSLOT Keys in request don't hash to the same slot`.
  2. `RedisLeaderElector::try_acquire` accesses `KEYS[1]` (`rume:leader:<role>`) and `KEYS[2]` (`rume:leader:term:<role>`). Without hash tags, calling `try_acquire` consistently fails with `CROSSSLOT`.
  3. `RedisCacheBackend::invalidate_tag` issues a multi-key `DEL` command containing `keys` (which are arbitrary cached keys across the system) alongside `tk` (`rume:tag:<tag>`). In Redis Cluster, cached keys are distributed across all cluster shards. A single multi-key `DEL` command containing keys across different slots fails with `CROSSSLOT`, causing tag invalidation to crash in clustered Redis topologies.
- **Suggested fix:**
  1. In `RedisDistributedLock`, wrap the resource identifier in curly braces `{...}`:
     ```rust
     fn lock_key(&self, resource: &str) -> String {
         format!("{}lock:{{{}}}", self.prefix, resource)
     }
     fn fence_key(&self, resource: &str) -> String {
         format!("{}fence:{{{}}}", self.prefix, resource)
     }
     ```
  2. In `RedisLeaderElector`, wrap the role identifier in curly braces `{...}`:
     ```rust
     fn leader_key(&self, role: &str) -> String {
         format!("{}leader:{{{}}}", self.prefix, role)
     }
     fn term_key(&self, role: &str) -> String {
         format!("{}term:{{{}}}", self.prefix, role)
     }
     ```
  3. In `RedisCacheBackend::invalidate_tag`, avoid a single multi-key `DEL` command across arbitrary keys. Instead, delete keys using a pipelined batch or execute single-key `DEL` commands in parallel, followed by deleting the tag set itself.

---

### C4 Cross-tenant job and idempotency key collisions permit cross-tenant rejection and result leakage

- **Where:** `crates/core/src/persistence/sql/postgres/jobs_schema.sql:1–21`, `crates/core/src/jobs/redis.rs:190–202, 230–257`, `crates/core/src/jobs/queue.rs:20–45`
- **What it says:**
  In `jobs_schema.sql`:
  ```sql
  CREATE TABLE IF NOT EXISTS rume_jobs (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL,
      payload BYTEA NOT NULL,
      ...
      idempotency_key TEXT,
      ...
  );

  CREATE UNIQUE INDEX IF NOT EXISTS idx_rume_jobs_idempotency
  ON rume_jobs (idempotency_key)
  WHERE idempotency_key IS NOT NULL AND status != 'dead_letter';
  ```
  In `RedisIdempotencyStore`:
  ```rust
  fn key(&self, key: &str) -> String {
      format!("{}{}", self.prefix, key)
  }
  ```
- **Why it's a problem:** In multi-tenant systems, jobs and idempotency keys submitted by distinct tenants frequently share identical business identifiers (e.g. `invoice-1001`, `user-sync-daily`, `order-502`). Neither `rume_jobs` nor `RedisIdempotencyStore` incorporates a tenant identifier into its idempotency namespace or schema indices:
  1. **Cross-tenant job denial of service:** When Tenant A enqueues a job with idempotency key `"invoice-1001"`, PostgreSQL inserts the row into `rume_jobs`. When Tenant B subsequently enqueues its own job with idempotency key `"invoice-1001"`, `idx_rume_jobs_idempotency` fails with a unique constraint violation. Tenant B's legitimate job is rejected due to Tenant A's unrelated activity.
  2. **Cross-tenant job payload data leakage:** In `RedisIdempotencyStore::try_acquire`, the cache key is `format!("rume:idempotency:{}", key)`. If Tenant A executes a job with result payload bytes, `complete()` caches `COMPLETED:<hex>`. When Tenant B enqueues a job with the same idempotency key, `try_acquire` returns `IdempotencyStatus::Completed(Some(tenant_a_bytes))`. Tenant B receives Tenant A's private computation output, causing severe cross-tenant data leakage.
- **Suggested fix:**
  1. In `rume_jobs`, add a `tenant_id: Option<String>` column. Update the unique index to compound tenant isolation:
     ```sql
     CREATE UNIQUE INDEX idx_rume_jobs_idempotency
     ON rume_jobs (COALESCE(tenant_id, ''), idempotency_key)
     WHERE idempotency_key IS NOT NULL AND status != 'dead_letter';
     ```
  2. In `IdempotencyStore`, require tenant scoping or length-prefixed formatting:
     ```rust
     fn scoped_idempotency_key(tenant_id: &str, key: &str) -> String {
         format!("tenant:{}:{}:idemp:{key}", tenant_id.len(), tenant_id)
     }
     ```

---

### M1 `TenantScopedCache` quota leak on TTL expiration permanently locks out tenant caching

- **Where:** `crates/core/src/tenant/cache.rs:101–126, 131–137, 142–150`, `crates/core/src/tenant/quota.rs:81–109`
- **What it says:**
  In `TenantScopedCache::set_with_tags`:
  ```rust
  let full_key = Self::scoped_key(tenant_id, key);
  let exists = self.cache.exists(&full_key).await?;

  if !exists && let Some(ref quota) = self.quota_store {
      let acquired = quota.acquire(tenant_id, self.max_keys_per_tenant).await?;
      if !acquired {
          return Err(CacheError::QuotaExceeded(format!(
              "tenant '{tenant_id}' exceeded quota of {} keys",
              self.max_keys_per_tenant
          )));
      }
  }
  ...
  self.cache.set_json_with_tags(&full_key, value, &tag_refs, ttl).await
  ```
  In `RedisTenantQuotaStore::acquire`:
  ```lua
  local current = redis.call('GET', KEYS[1])
  if current and tonumber(current) >= tonumber(ARGV[1]) then
      return 0
  else
      redis.call('INCR', KEYS[1])
      return 1
  end
  ```
- **Why it's a problem:** In `TenantScopedCache`, quota permits are incremented via `quota.acquire()` whenever a new key is written. However, permits are only decremented during explicit programmatic calls to `delete()`, `invalidate_tag()`, or `invalidate_tenant()`.
  1. In standard caching workloads, keys are stored with a TTL (e.g. 60 seconds). When a cached key expires via TTL in Redis or memory, the cache backend passively evicts it.
  2. Neither Redis nor the cache backend notifies `TenantQuotaStore` when a key expires. The counter in `RedisTenantQuotaStore` (`rume:quota:<tenant_id>`) retains its value and never decrements.
  3. After writing `max_keys_per_tenant` (default 5,000) keys over any period of time, the tenant's counter hits the maximum quota ceiling. Any subsequent `set()` fails with `CacheError::QuotaExceeded`, **even when every single previous key has expired and the cache is completely empty for that tenant**. The tenant is permanently locked out of caching until an administrator manually calls `invalidate_tenant()`.
  4. Furthermore, if `set_json_with_tags` fails or is cancelled after `quota.acquire()` succeeds, the acquired permit is never released, leaking permits under network errors.
- **Suggested fix:**
  1. Maintain active tenant keys in a Redis Sorted Set scored by expiration timestamp (`ZADD tenant:keys <expires_at> <key>`). In `acquire()`, prune expired keys via `ZREMRANGEBYSCORE tenant:keys -inf <now>` before checking `ZCARD` against `max_keys`.
  2. Implement an RAII quota permit or wrap `quota.acquire()` so that if `set_json_with_tags` fails or is cancelled, `quota.release()` is invoked immediately in a fallback block.

---

### M2 Monotonic memory leak in Redis tag invalidation sets from expired TTL keys

- **Where:** `crates/core/src/cache/redis.rs:129–158, 199–234`
- **What it says:**
  ```rust
  let tag_keys: Vec<String> = tags.iter().map(|t| self.tag_set_key(t)).collect();
  ...
  let mut cmd = redis::cmd("SET");
  cmd.arg(&k).arg(&value);
  if let Some(ttl) = ttl {
      cmd.arg("PX").arg(ttl.as_millis() as u64);
  }
  cmd.query_async::<()>(&mut *conn).await?;

  for tk in tag_keys {
      redis::cmd("SADD")
          .arg(&tk)
          .arg(&k)
          .query_async::<()>(&mut *conn)
          .await?;
  }
  ```
- **Why it's a problem:** When caching keys with a TTL (`PX`), the key `k` is automatically evicted by Redis when its expiration deadline passes. However, the Redis Set `rume:tag:<tag>` holding references to `k` is populated via `SADD` with **no TTL and no automatic eviction**.
  1. Dead key references accumulate in the tag set indefinitely. For high-throughput caching (e.g. session tokens, query caches, user profiles), millions of expired key names remain stored in Redis Sets, causing continuous monotonic memory growth.
  2. When `invalidate_tag()` is subsequently invoked, `SMEMBERS` retrieves millions of non-existent key strings across the network, saturating socket buffers and executing useless `DEL` commands on keys that have long since vanished.
- **Suggested fix:**
  1. Whenever `set_with_tags` is called, update the expiration of the tag set using `EXPIRE` or `PEXPIRE` to match the maximum TTL of its member keys plus a buffer.
  2. In `invalidate_tag`, prune stale keys or use Redis Sorted Sets scored by expiration timestamp, periodically running `ZREMRANGEBYSCORE` to purge expired references.

---

### M3 Delimiter collision in `RedisLeaderElector` candidate IDs breaks leader recovery

- **Where:** `crates/core/src/coordination/redis.rs:273, 325–329, 372–383, 403–407`
- **What it says:**
  In `try_acquire`:
  ```lua
  local val = ARGV[1] .. ':' .. tostring(term)
  redis.call('SET', KEYS[1], val, 'PX', ARGV[2])
  ```
  In `renew` and `resign`:
  ```lua
  local prefix = ARGV[1] .. ':'
  if string.sub(current, 1, string.len(prefix)) == prefix then ...
  ```
  In `get_leader`:
  ```rust
  if let Some(s) = val
      && let Some((candidate, term_str)) = s.split_once(':')
      && let Ok(term) = term_str.parse::<u64>()
  {
      Ok(Some(LeaderLease {
          role: r,
          candidate_id: candidate.to_string(),
          term,
      }))
  } else {
      Ok(None)
  }
  ```
- **Why it's a problem:** Candidate IDs and terms are stored as a raw concatenated string `candidate_id .. ':' .. term`.
  1. In production environments, candidate IDs frequently contain colons (e.g. host-port pairs `10.244.1.12:8080`, socket strings, or namespace-qualified IDs `k8s:pod:scheduler-0`).
  2. When candidate ID is `"node:1"` and term is `5`, the stored Redis value is `"node:1:5"`.
  3. In `get_leader()`, `s.split_once(':')` splits on the *first* colon: `candidate` becomes `"node"`, and `term_str` becomes `"1:5"`. `term_str.parse::<u64>()` fails. `get_leader()` returns `None`, falsely reporting that no active leader exists while Node 1 is actively holding leadership.
  4. In `renew()` and `resign()`, `string.sub(current, 1, string.len(prefix)) == prefix` checks prefix matching. If Candidate A is `"worker"` and Candidate B is `"worker:node"`, Candidate A's prefix `"worker:"` matches Candidate B's key, allowing Candidate A to resign or renew Candidate B's leadership.
- **Suggested fix:**
  1. In `get_leader`, split on the *last* colon using `s.rsplit_once(':')`.
  2. Even better, length-prefix the candidate ID (`format!("{}:{}:{}", candidate_id.len(), candidate_id, term)`) or store leadership data in a Redis Hash (`HSET rume:leader:{role} candidate <id> term <term>`).

---

### M4 Global lock contention and O(N) eviction loop in `RateLimiter::check`

- **Where:** `crates/core/src/resilience/rate_limiter.rs:66–98`
- **What it says:**
  ```rust
  pub fn check(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
      let mut clients = self.clients.write().unwrap();
      let now = Instant::now();
      let window_start = now.checked_sub(self.config.window).unwrap_or(now);

      let entry = clients.entry(key.to_string()).or_default();
      entry.timestamps.retain(|&t| t > window_start);
      ...
      // Passive eviction of inactive clients to bound memory usage
      if clients.len() > 256 {
          clients.retain(|_, e| {
              e.timestamps.retain(|&t| t > window_start);
              !e.timestamps.is_empty()
          });
      }
  ```
- **Why it's a problem:**
  1. **Multicore lock serialization:** `self.clients.write().unwrap()` acquires an exclusive write lock on every single incoming HTTP request across all worker threads. Under high-throughput conditions (e.g. 10,000 req/sec), all CPU cores stall waiting for this single mutex.
  2. **Unbounded O(N) loop under lock:** Once the server encounters more than 256 unique client keys (which happens in seconds under public internet traffic), **every single request** executes `clients.retain(...)`, iterating over all entries and filtering each client's timestamp vector while holding the exclusive lock. This turns request processing from $O(1)$ into $O(N)$ with lock contention, causing catastrophic latency spikes.
  3. **Multi-hop IP extraction:** `RateLimitService` extracts raw `X-Forwarded-For` without parsing comma-delimited proxy hops. A client whose requests traverse different proxy routes generates a new map entry for each distinct header value (`"203.0.113.1, 10.0.0.1"` vs `"203.0.113.1, 10.0.0.2"`), rapidly filling the map.
- **Suggested fix:**
  1. Replace `Arc<RwLock<BTreeMap>>` with a concurrent striped map like `dashmap::DashMap`.
  2. Remove inline `clients.retain()` from the request path. Run `prune_expired()` on a periodic background task (e.g. every 30 seconds) rather than running it inside `check()`.
  3. Parse `X-Forwarded-For` by extracting and trimming the first IP address.

---

### M5 Complete lack of multi-tenant resource isolation in rate limiting and database pools

- **Where:** `crates/core/src/resilience/rate_limiter.rs:168–175`, `crates/core/src/tenant/layer.rs:83–101`, `crates/core/src/persistence/pool.rs:18–48`
- **What it says:**
  In `RateLimitService::call`:
  ```rust
  let client_key = req
      .headers()
      .get("x-forwarded-for")
      .and_then(|h| h.to_str().ok())
      .unwrap_or("client_default")
      .to_string();
  ```
  In `TenantService::call`:
  ```rust
  match resolver.resolve(&parts).await {
      Ok(Some(tenant)) => {
          let mut req = Request::from_parts(parts, body);
          req.extensions_mut().insert(tenant);
          inner.call(req).await
      }
  ...
  ```
- **Why it's a problem:** Multi-tenancy requires strict boundary isolation across compute, storage, cache, and rate limiting:
  1. `RateLimitLayer` has no tenant awareness: it checks only client IP from `X-Forwarded-For`. When multiple tenants share an internal network or proxy, all requests collapse into `"client_default"`. A burst of requests from Tenant A triggers 429 rejections for Tenant B. There is no tenant-scoped rate limiter or quota layer that enforces per-tenant request limits.
  2. `DatabasePool` and persistence backends (`PostgresBackend`, `MysqlBackend`, `SqliteBackend`) have no tenant awareness or context propagation. There is no support for schema-per-tenant (`SET LOCAL search_path = tenant_<id>`), database-per-tenant, or automatic row-level tenant filtering.
- **Suggested fix:**
  1. In `RateLimitLayer`, check for `TenantContext` or `Tenant` in request extensions before falling back to `x-forwarded-for`. Allow configuring rate limit policies per tenant tier.
  2. Provide a tenant-aware database connection adapter or session hook in `DatabasePool` that sets the PostgreSQL tenant schema/session variable upon acquiring a lease.

---

### M6 `HeaderTenantResolver::add_tenant` silently overwrites conflicting slugs and precedence shadows vanity slugs

- **Where:** `crates/core/src/tenant/resolver.rs:54–62, 85–109`
- **What it says:**
  ```rust
  pub fn add_tenant(self, tenant: Tenant) -> Self {
      if let Ok(mut id_map) = self.by_id.write() {
          id_map.insert(tenant.id.clone(), tenant.clone());
      }
      if let Ok(mut slug_map) = self.by_slug.write() {
          slug_map.insert(tenant.slug.clone(), tenant);
      }
      self
  }
  ```
  In `resolve`:
  ```rust
  if let Some(tenant) = id_lookup {
      return Ok(Some(tenant));
  }
  let slug_map = self.by_slug.read().map_err(...)?;
  if let Some(tenant) = slug_map.get(key) {
      Ok(Some(tenant.clone()))
  } else {
      Err(TenantError::TenantNotFound(key.to_string()))
  }
  ```
- **Why it's a problem:**
  1. **Silent slug hijacking:** If Tenant 1 registers with slug `"acme"`, and Tenant 2 registers with the same slug `"acme"`, `slug_map.insert` silently overwrites Tenant 1's entry. Requests specifying `X-Tenant-ID: acme` are silently routed to Tenant 2 without error or warning.
  2. **Vanity slug shadowing:** Because `by_id` lookup always precedes `by_slug`, if Tenant A has ID `"corp"` and Tenant B has slug `"corp"`, Tenant B's users can never access their tenant via `X-Tenant-ID: corp`—it will always resolve to Tenant A.
- **Suggested fix:** In `add_tenant`, validate that `tenant.slug` is not already present in `by_slug` or `by_id`. Return `Result<Self, TenantError>` or reject conflicting slug registrations to prevent tenant route hijacking.

---

### m1 Subdomain tenant resolver fails on case-insensitive hosts and IPv6 addresses

- **Where:** `crates/core/src/tenant/resolver.rs:153–175`
- **What it says:**
  ```rust
  let host_no_port = host_str.split(':').next().unwrap_or(host_str);
  ...
  if let Some(subdomain) = host_no_port.strip_suffix(&normalized_base) {
      ...
      if let Some(tenant) = map.get(subdomain) {
  ```
- **Why it's a problem:**
  1. Per RFC 7230 §5.4, HTTP Host headers are case-insensitive. A request with `Host: Acme.app.platform.io` extracts `"Acme"`, which fails `map.get("Acme")` because the slug was registered as lowercase `"acme"`.
  2. `host_str.split(':').next()` splits IPv6 addresses (e.g. `[::1]:8080`) at the first colon, producing `[` and failing resolution.
- **Suggested fix:** Convert `host_str` to lowercase before stripping the base domain, and strip optional ports by checking for `]` when parsing IPv6 literals.

---

### m2 `RedisRateLimiter` misclassifies Redis connection and script errors as `CircuitOpen`

- **Where:** `crates/core/src/resilience/redis.rs:60, 104`
- **What it says:**
  ```rust
  let mut conn = self
      .pool
      .get()
      .await
      .map_err(|_| ResilienceError::CircuitOpen)?;
  ...
  let res: (i64, i64, i64) = script
      .invoke_async(&mut *conn)
      .await
      .map_err(|_| ResilienceError::CircuitOpen)?;
  ```
- **Why it's a problem:** When Redis is unreachable, deadpool-redis connection checkout fails, or a Lua script fails, `RedisRateLimiter` maps the error to `ResilienceError::CircuitOpen`. This causes `AppError` to respond with `503 SERVICE_UNAVAILABLE` and error code `CIRCUIT_BREAKER_OPEN`. Rate limiting failures have no relation to circuit breakers, polluting operational telemetry and falsely reporting downstream circuit breaker trips.
- **Suggested fix:** Add `ResilienceError::Backend(String)` to `ResilienceError`, or fail open with a logged warning when the rate-limiting backend is unavailable.

---

### m3 `LockGuard` and `LeaderLease` lack expiration timestamp telemetry

- **Where:** `crates/core/src/coordination/lock.rs:16–23`, `crates/core/src/coordination/leader.rs:9–17`
- **What it says:**
  ```rust
  pub struct LockGuard {
      pub resource: String,
      pub lease_id: String,
      pub fencing_token: u64,
  }

  pub struct LeaderLease {
      pub role: String,
      pub candidate_id: String,
      pub term: u64,
  }
  ```
- **Why it's a problem:** Neither guard structure carries an expiration timestamp or remaining TTL duration. Callers executing long-running asynchronous routines cannot inspect how much time remains on their lease before triggering auto-renewal or aborting to prevent split-brain execution.
- **Suggested fix:** Add `pub expires_at: Instant` (or epoch milliseconds) to `LockGuard` and `LeaderLease`, alongside helper methods `is_expired()` and `time_remaining()`.

---

### m4 Unused global sequence burn in `PostgresDistributedLock::acquire` conflict path

- **Where:** `crates/core/src/persistence/sql/postgres/lock_acquire.sql:1–8`
- **What it says:**
  ```sql
  INSERT INTO rume_locks (resource, lease_id, fencing_token, expires_at)
  VALUES ($1, $2, nextval('rume_lock_fencing_seq'), $3)
  ON CONFLICT (resource) DO UPDATE
  SET lease_id = EXCLUDED.lease_id,
      fencing_token = nextval('rume_lock_fencing_seq'),
      expires_at = EXCLUDED.expires_at
  WHERE rume_locks.expires_at <= $4
  RETURNING fencing_token;
  ```
- **Why it's a problem:** When a lock row already exists, `nextval` in the `VALUES` clause is evaluated before conflict resolution, and then `nextval` in `DO UPDATE` is evaluated again. This consumes two sequence values per successful conflict update, and burns a sequence value even when acquisition fails completely (`expires_at <= $4` is false).
- **Suggested fix:** Compute the next fencing token inside `DO UPDATE` or pass the sequence explicitly to prevent sequence burning on rejected attempts.

---

### m5 Expired rows in `rume_locks` and `rume_leaders` are retained indefinitely until next access

- **Where:** `crates/core/src/persistence/sql/postgres/locks_schema.sql`, `leader_schema.sql`
- **What it says:** `rume_locks` and `rume_leaders` use `PRIMARY KEY (resource)` and `PRIMARY KEY (role)` without indexes on `expires_at` or background pruning.
- **Why it's a problem:** In systems with dynamically generated ephemeral lock resource names (e.g. `lock:user:123`, `lock:export:456`), expired rows remain in PostgreSQL indefinitely unless re-acquired, creating unbounded table bloat.
- **Suggested fix:** Add `CREATE INDEX IF NOT EXISTS idx_rume_locks_expires_at ON rume_locks (expires_at)` and provide a background cleanup function `prune_expired()`.

---

## Positive notes

- **Strictly monotonic fencing tokens and term sequence tracking:**
  - `PostgresDistributedLock` issues monotonic fencing tokens via PostgreSQL sequences (`rume_lock_fencing_seq`), and `RedisDistributedLock` issues monotonic fencing tokens via atomic Redis `INCR`, providing correct fencing counters to prevent split-brain storage writes.
- **Length-prefixed tenant cache keys and scoped tags:**
  - `scoped_cache_key` (`format!("tenant:{}:{}:{key}", tenant_id.len(), tenant_id)`) and `tenant_cache_tag` eliminate colon-delimiter collision attacks between tenant ID and key segments.
- **Strict DNS boundary validation in `SubdomainTenantResolver`:**
  - Normalizes base domains with a leading dot and verifies single-label subdomain syntax (`!subdomain.contains('.')` and `c.is_ascii_alphanumeric() || c == '-'`), completely preventing suffix spoofing (`evilapp.platform.io`).
- **Namespace segregation in `HeaderTenantResolver`:**
  - `by_id` and `by_slug` are segregated into separate lookup maps, preventing registration order from overwriting primary IDs with vanity slugs.
- **Clean Axum extractor ergonomics:**
  - `TenantContext` and `OptionalTenant` integrate smoothly with Axum 0.8 / Tower layers, returning RFC 9457 Problem Details (`MISSING_TENANT`, `TENANT_NOT_FOUND`) with zero boilerplate in HTTP handlers.
- **Non-blocking Redis Lua coordination:**
  - Redis distributed locks and leader election execute atomic state checks and lease expirations inside Redis Lua scripts without multi-roundtrip network races.

---

## Verdict

**Not ready for production without a targeted remediation pass.**

While the coordination interfaces and multi-tenant abstractions are well-structured, the implementation contains **4 Critical findings** and **6 Major findings** that will cause split-brain execution under clock drift, cluster-wide coordination failures on Redis Cluster, cross-tenant job collisions, and permanent tenant caching lockouts:

1. **C1** — Replace client-side `current_epoch_millis()` in PostgreSQL coordination queries with database-authoritative `(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT` to eliminate clock-skewed lock and leadership theft.
2. **C2** — Advance election terms unconditionally when re-acquiring expired leases in `leader_acquire.sql` to maintain monotonic fencing across crashes and pauses.
3. **C3** — Add Redis hash tags `{...}` to lock, term, and leader keys in `crates/core/src/coordination/redis.rs` and pipeline `DEL` in `invalidate_tag` to eliminate `CROSSSLOT` crashes on Redis Cluster.
4. **C4** — Scope job queues and `RedisIdempotencyStore` by tenant ID to prevent cross-tenant job rejection and payload result leakage.
5. **M1** — Reconcile tenant cache quotas with TTL expirations and handle request cancellations to prevent permanent tenant cache lockouts.
6. **M2** — Set TTLs or prune expired key references in Redis tag sets to prevent unbounded Redis RAM growth.
7. **M3** — Use `rsplit_once(':')` or length prefixing in `RedisLeaderElector` to support candidate IDs containing colons.
8. **M4** — Replace the global `RwLock<BTreeMap>` and inline $O(N)$ eviction in `RateLimiter` with a concurrent striped map and periodic background pruning.
9. **M5** — Add tenant context propagation to rate limiting middleware and database connection pools.
10. **M6** — Validate slug uniqueness in `HeaderTenantResolver::add_tenant` to prevent silent route hijacking.
