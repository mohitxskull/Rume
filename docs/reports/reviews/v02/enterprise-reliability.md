# Enterprise Reliability Review — v02

## Summary

The Rume v02 codebase introduces foundational enterprise capabilities: circuit breakers, sliding-window rate limiters, concurrency limiters with RAII load-shedding permits, distributed locks with monotonic fencing tokens, leader election interfaces, transactional outbox relays, and Prometheus metrics exposition. However, serious operational reliability and high-availability vulnerabilities prevent the platform from operating reliably at enterprise scale. Most critically: Prometheus metrics recording stores every request duration in an unbounded heap vector causing process-killing OOMs and scrape latency stalls; concurrent outbox relays lack message-claiming locks or leader election guards, causing duplicated message publication across high-availability replicas; background workers lack shed-load concurrency bounds and can spawn unbounded tasks during queue spikes; distributed worker heartbeats and abandoned job recovery remain disconnected dead code; the in-memory cache backend never evicts expired entries, leaking memory indefinitely; and cache-aside `remember` lacks stampede (thundering herd) protection. Rume is not yet ready for production scale without a remediation pass.

## Prior decisions

In the v01 review cycle, enterprise reliability was not evaluated as a dedicated lens. The following prior decisions from `docs/reports/reviews/v01/` intersect with operational reliability, high availability, and lifecycle resilience:

- **D-M3 (Overlapping `health_routes()` and `ops_router()` health APIs)** — **REAFFIRMED**. Verified in `crates/core/src/routing/health.rs` and `crates/core/src/app/shutdown.rs:58–88`. The two divergent endpoints still coexist with disjoint state models, creating operational ambiguity for Kubernetes readiness and liveness probe configuration. Tracked in Stage 1 Architecture Review finding `m1`.
- **C-m4 (`/health/ready` returns 503 for `Warning` status)** — **REAFFIRMED**. Verified in `crates/core/src/routing/health.rs:140–144, 243–248`. `is_healthy` remains false for `Warning` status, enforcing strict readiness fail-closed behavior at the ingress load balancer during degraded subsystem operation.
- **C-C3 / m2 (`Lifecycle` and `Availability` state discriminant safety)** — **REAFFIRMED**. Verified in `crates/core/src/app/state.rs:47–54, 114–121`. Discriminants are pinned via `#[repr(u8)]` with `MAX_DISCRIMINANT` validation, guaranteeing that atomic availability transitions remain sound across compiler reorderings.
- **Resilience middleware (`circuit_breaker`, `concurrency`, `rate_limiter`, `timeout`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in findings M3, Positive Notes, and Stage 1 Correctness Review findings M1 and m2.
- **Distributed worker heartbeats & job reliability (`jobs/reliability.rs`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in findings M1 and M2.
- **Transactional outbox pattern & relay (`outbox/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in findings C2, m2, and m3.
- **Cache-aside, TTL eviction, and tag-based invalidation (`cache/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in findings M4 and M5.
- **Metrics recording and Prometheus exposition (`metrics/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in findings C1, m1, and Positive Notes.
- **Multi-tenant resource boundaries (`tenant/cache.rs`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in finding m4 (and Stage 1 Security Review finding C2).
- **Leader election failover and distributed locks (`coordination/`)** — **NOTE-NEW**. New subsystem introduced in v02; evaluated in finding M6 and Positive Notes.

---

## Findings

### C1 Prometheus metrics recording stores unbounded raw float samples, causing heap memory exhaustion (OOM) and scraping stalls

- **Where:** `crates/core/src/metrics/registry.rs:57, 73–80, 134–149`
- **What it says:**
  ```rust
  #[derive(Debug, Clone, Default)]
  pub struct MetricsRegistry {
      requests_total: Arc<RwLock<BTreeMap<(String, u16), u64>>>,
      request_latencies: Arc<RwLock<BTreeMap<String, Vec<f64>>>>,
      in_flight: Arc<AtomicI64>,
  }
  ...
  pub fn record_http_request(&self, method: &str, status: u16, duration: Duration) {
      if let Ok(mut total) = self.requests_total.write() {
          *total.entry((method.to_uppercase(), status)).or_default() += 1;
      }

      let secs = duration.as_secs_f64();
      if let Ok(mut latencies) = self.request_latencies.write() {
          latencies
              .entry(method.to_uppercase())
              .or_default()
              .push(secs);
      }
  }
  ...
  if let Ok(latencies) = self.request_latencies.read() {
      for (method, samples) in latencies.iter() {
          let count = samples.len();
          let sum: f64 = samples.iter().sum();
          writeln!(
              out,
              "http_request_duration_seconds_count{{method=\"{method}\"}} {count}"
          )
          .unwrap();
          writeln!(
              out,
              "http_request_duration_seconds_sum{{method=\"{method}\"}} {sum:.6}"
          )
          .unwrap();
      }
  }
  ```
- **Why it's a problem:** Every single HTTP request handled by the server pushes an 8-byte floating point duration into an unbounded `Vec<f64>` on heap memory that is **never cleared, pruned, histogram-bucketed, or bounded**.
  1. **Monotonic memory leak:** At a modest enterprise throughput of 5,000 requests per second, the registry accumulates 300,000 samples per minute (~2.4 MB/min, ~144 MB/hour, ~3.5 GB/day) in heap allocations per method. Within days or hours of production traffic, the process will exceed container memory limits and be killed by the Linux OOM killer.
  2. **Scraping latency degradation:** On every scrape of `/metrics`, `render_prometheus()` acquires a read lock on the map and computes `.iter().sum()` over all accumulated samples. As the vector grows into millions of entries, scrape request handling stalls the async runtime, saturates CPU cores, causes Prometheus scrape timeouts (default 10s), and triggers false-positive node-down alerts.
  3. **High write lock contention:** `record_http_request` takes an exclusive write lock on `request_latencies` on every single HTTP request completion across all worker threads.
- **Suggested fix:** Replace the unbounded raw sample vector with standard Prometheus counters and fixed histogram buckets:
  1. Store running accumulators: `count: AtomicU64` and `sum_millis: AtomicU64` (or `AtomicF64`).
  2. For latency distribution, maintain fixed histogram bucket boundaries (e.g. `[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]`) with atomic counters (`Vec<AtomicU64>`), or use the `metrics` / `prometheus-client` crate.
  3. This guarantees $O(1)$ constant memory and $O(1)$ scrape computation regardless of traffic volume.

---

### C2 Concurrent `OutboxRelay` instances lack claiming locks, causing duplicate message publication across HA replicas

- **Where:** `crates/core/src/outbox/relay.rs:59–78, 83–89` and `crates/core/src/outbox/repository.rs:20–25, 64–83`
- **What it says:**
  In `crates/core/src/outbox/relay.rs`:
  ```rust
  pub async fn poll_and_dispatch(&self, batch_size: usize) -> Result<usize, OutboxError> {
      let pending = self.repository.fetch_pending(batch_size).await?;
      let mut published_count = 0;

      for message in pending {
          match (self.dispatcher)(&message).await {
              Ok(()) => {
                  self.repository.mark_published(&message.id).await?;
                  published_count += 1;
              }
              Err(err) => {
                  self.repository
                      .mark_failed(&message.id, err.to_string())
                      .await?;
              }
          }
      }

      Ok(published_count)
  }
  ```
  In `crates/core/src/outbox/repository.rs`:
  ```rust
  fn fetch_pending(
      &self,
      limit: usize,
  ) -> BoxFuture<'_, Result<Vec<OutboxMessage>, OutboxError>> {
      Box::pin(async move {
          let messages = self
              .messages
              .read()
              .map_err(|e| OutboxError::Repository(e.to_string()))?;

          let pending = messages
              .values()
              .filter(|m| m.status == OutboxStatus::Pending)
              .take(limit)
              .cloned()
              .collect();

          Ok(pending)
      })
  }
  ```
- **Why it's a problem:** In an enterprise high availability deployment, multiple instances (replicas) of the application run concurrently behind an ingress load balancer.
  1. **Duplicate publication:** If two instances run `OutboxRelay::poll_and_dispatch` concurrently, both instances call `fetch_pending` simultaneously. Because `fetch_pending` merely filters `status == OutboxStatus::Pending` under a read lock without acquiring a row-level lock or transitioning the status to an intermediate in-flight state (e.g. `InFlight` / `Sending`), **both instances fetch the exact same batch of messages**.
  2. Both instances invoke `(self.dispatcher)(&message).await` concurrently, transmitting duplicate events (e.g. `order.paid`, `invoice.generate`, `email.welcome`) to external message brokers (Kafka, RabbitMQ, SQS) or webhooks.
  3. **Lack of leader coordination:** `OutboxRelay` does not integrate with `LeaderElector` or `DistributedLock` to ensure that only a single active leader node executes the relay.
  4. **Lack of transactional atomicity:** `OutboxRepository` defines `save(&self, msg: OutboxMessage)`, but provides no mechanism (`save_tx`) to participate in a database transaction (`DatabaseTransaction`). An outbox record cannot be committed atomically alongside domain entity state changes, violating the core transactional outbox pattern contract.
- **Suggested fix:**
  1. Add an intermediate `OutboxStatus::InFlight { locked_until: Instant }` state or support transactional row locking (`SELECT ... FOR UPDATE SKIP LOCKED` in SQL). When messages are fetched, they must be atomically claimed so peer relay workers skip them.
  2. Guard relay polling loops with `LeaderElector` or a periodic distributed lock lease so that only the primary leader replica executes the outbox relay in active-passive mode.
  3. Provide `save_tx(&self, tx: &mut dyn Transaction, msg: OutboxMessage)` in `OutboxRepository` so that writing the outbox message is atomic with business transactions.

---

### M1 `JobWorker` lacks concurrency bounds, spawning unbounded Tokio tasks during queue spikes

- **Where:** `crates/core/src/jobs/worker.rs:130–151`
- **What it says:**
  ```rust
  async fn run_loop(&self) {
      while !self.is_stopping.load(Ordering::Relaxed) {
          match self.queue.dequeue().await {
              Ok(Some(job)) => {
                  self.in_flight_count.fetch_add(1, Ordering::SeqCst);
                  let queue = Arc::clone(&self.queue);
                  let registry = Arc::clone(&self.registry);
                  let in_flight = Arc::clone(&self.in_flight_count);

                  tokio::spawn(async move {
                      Self::execute_job(queue, registry, job).await;
                      in_flight.fetch_sub(1, Ordering::SeqCst);
                  });
              }
              Ok(None) => {
                  sleep(Duration::from_millis(25)).await;
              }
              Err(_) => {
                  sleep(Duration::from_millis(50)).await;
              }
          }
      }
  }
  ```
- **Why it's a problem:** While Rume provides `ConcurrencyLimitLayer` for HTTP endpoints, `JobWorker` has **no concurrency bounding or worker pool size limits**.
  If an upstream job producer enqueues 20,000 background jobs (e.g. batch notification dispatch, data ingestion, mass email campaign), `JobWorker::run_loop` dequeues them in a tight loop and immediately calls `tokio::spawn` for every single job without pause.
  Within fractions of a second, tens of thousands of active async tasks will be spawned. Each task acquires database connections, opens network sockets, and allocates memory, leading to Tokio task scheduler thrashing, database pool connection exhaustion, file descriptor exhaustion, and process crashes.
- **Suggested fix:** Add a configurable `concurrency: usize` limit to `JobWorker` (e.g. `JobWorkerConfig { max_concurrency: usize }`, defaulting to 10 or `num_cpus * 2`), and bound concurrent executions using an `Arc<tokio::sync::Semaphore>` before calling `dequeue()`. Only acquire and dequeue a new job when a concurrency permit is available.

---

### M2 Distributed worker heartbeats and abandoned job recovery are disconnected dead code

- **Where:** `crates/core/src/jobs/reliability.rs:135–224` and `crates/core/src/jobs/worker.rs:79–151`
- **What it says:**
  The module doc of `crates/core/src/jobs/reliability.rs` states:
  `//! Job reliability: idempotency keys, worker heartbeats, and abandoned job recovery.`
  The file defines:
  ```rust
  pub trait HeartbeatRegistry: Debug + Send + Sync + 'static {
      fn record_heartbeat(&self, worker_id: &str, ttl: Duration) -> BoxFuture<'_, Result<(), JobError>>;
      fn is_alive(&self, worker_id: &str) -> BoxFuture<'_, Result<bool, JobError>>;
      fn reap_dead_workers(&self) -> BoxFuture<'_, Result<Vec<String>, JobError>>;
  }
  ```
  However, across the entire codebase:
  1. `JobWorker` does not record heartbeats, has no worker ID, and does not spawn a background heartbeat renewal loop.
  2. Neither `JobWorker` nor `JobQueue` ever invokes `HeartbeatRegistry`.
  3. No abandoned job reaper task exists to query `reap_dead_workers()` and return abandoned in-flight jobs to the queue.
  4. `IdempotencyStore` is similarly never invoked by `JobWorker::execute_job` or `JobRegistry`.
- **Why it's a problem:** In distributed worker architectures, worker nodes crash, lose network access, or suffer hardware failures while jobs are marked in-flight. Without heartbeats and abandoned job recovery, jobs claimed by a failed worker node remain permanently stuck in `in_flight` and are lost from processing. The codebase implements the registry primitives but never connects them to the worker runtime.
- **Suggested fix:**
  1. Assign each `JobWorker` a unique `worker_id: String` (e.g. `hostname-uuid`).
  2. When a `HeartbeatRegistry` is supplied to `JobWorker`, spawn a background heartbeat task that periodically calls `record_heartbeat(worker_id, ttl)`.
  3. In `JobWorker` or a supervisor process, run an abandoned job recovery loop that calls `reap_dead_workers()`, queries the queue for jobs owned by reaped workers, and re-enqueues (`nack`) or resets them for processing.
  4. Wire `IdempotencyStore` into `JobWorker::execute_job` using the job's idempotency key if specified.

---

### M3 `RateLimiter` permanently leaks memory for inactive clients and serializes all requests on a global `RwLock`

- **Where:** `crates/core/src/resilience/rate_limiter.rs:50–53, 65–96, 148–155`
- **What it says:**
  ```rust
  pub struct RateLimiter {
      config: RateLimitConfig,
      clients: Arc<RwLock<BTreeMap<String, RateLimitEntry>>>,
  }
  ...
  pub fn check(&self, key: &str) -> Result<RateLimitStatus, ResilienceError> {
      let mut clients = self.clients.write().unwrap();
      let now = Instant::now();
      let window_start = now.checked_sub(self.config.window).unwrap_or(now);

      let entry = clients.entry(key.to_string()).or_default();
      // Discard timestamps older than the sliding window
      entry.timestamps.retain(|&t| t > window_start);
      ...
  ```
  and `RateLimitService::call`:
  ```rust
  let client_key = req
      .headers()
      .get("x-forwarded-for")
      .and_then(|h| h.to_str().ok())
      .unwrap_or("client_default")
      .to_string();
  ```
- **Why it's a problem:**
  1. **Unbounded memory leak:** `entry.timestamps.retain(...)` prunes timestamps older than the sliding window, but the client key itself in `clients: BTreeMap<String, RateLimitEntry>` is **never removed**, even when `entry.timestamps` is completely empty. In a public internet service where clients connect from thousands of dynamic IP addresses, the map grows monotonically without bound, consuming heap memory indefinitely.
  2. **Severe lock contention:** `self.clients.write().unwrap()` is acquired on **every single request** across all threads and CPU cores, turning the rate limiter into a global serialization bottleneck that throttles multicore throughput.
  3. **Collapsing global fallback:** When `X-Forwarded-For` is absent (e.g. direct clients, intranet traffic, or proxies using `Forwarded` / `X-Real-IP`), all requests fall back to `"client_default"`. All clients globally share a single rate-limiting bucket of 100 requests per minute, causing immediate false-positive 429 rejections for legitimate users.
  4. **Spoofing vulnerability:** Because `client_key` reads the raw `X-Forwarded-For` header without proxy validation, an attacker can bypass rate limiting entirely by sending randomized `X-Forwarded-For: <fake-ip>` headers on each request.
- **Suggested fix:**
  1. Periodically prune empty client entries from `clients`, or remove the entry immediately inside `check` if `entry.timestamps.is_empty()` after the window passes.
  2. Replace coarse-grained `RwLock<BTreeMap>` with a sharded map (`dashmap`) or per-client atomics/token buckets to eliminate cross-client lock contention.
  3. Support configurable client key extraction (e.g. authenticated user ID, tenant ID, or validated peer socket IP) rather than unvalidated `X-Forwarded-For`.

---

### M4 `MemoryCacheBackend` retains expired entries indefinitely without TTL eviction sweep or memory bounds

- **Where:** `crates/core/src/cache/backend.rs:60–67, 136–144, 169–194`
- **What it says:**
  ```rust
  impl CacheEntry {
      fn is_expired(&self) -> bool {
          if let Some(exp) = self.expires_at {
              Instant::now() >= exp
          } else {
              false
          }
      }
  }
  ...
  if let Some(entry) = state.entries.get(&k) {
      if entry.is_expired() {
          Ok(None)
      } else {
          Ok(Some(entry.value.clone()))
      }
  } else {
      Ok(None)
  }
  ```
  and `set_with_tags`:
  ```rust
  for tag in &tags_set {
      state
          .tag_to_keys
          .entry(tag.clone())
          .or_default()
          .insert(k.clone());
  }

  let entry = CacheEntry {
      value,
      expires_at,
      tags: tags_set,
  };

  state.entries.insert(k, entry);
  ```
- **Why it's a problem:**
  1. **No TTL memory cleanup:** `get()` checks `entry.is_expired()` and returns `Ok(None)`, but does not delete the expired entry from `state.entries` or remove keys from `state.tag_to_keys`. There is no active TTL eviction sweep, no passive eviction on read, and no maximum capacity/LRU eviction. An application caching short-lived items (tokens, query results, API responses) with a 60-second TTL will retain all expired byte buffers in RAM indefinitely, leading to memory exhaustion.
  2. **Stale tag associations on overwrite:** In `set_with_tags`, if key `k` was previously stored with tags `["tag1", "tag2"]` and is overwritten with `["tag3"]`, `set_with_tags` inserts `k` into `tag_to_keys["tag3"]` and overwrites `entries[k]`. However, it does not remove `k` from `tag_to_keys["tag1"]` or `tag_to_keys["tag2"]`. If `invalidate_tag("tag1")` is called later, `k` is erroneously invalidated even though it was decoupled from `tag1`.
- **Suggested fix:**
  1. Implement passive deletion: in `get()` and `exists()`, if an entry is expired, upgrade to a write lock (or take a write lock) and remove the expired key and its tag references.
  2. Provide a background eviction sweep task or enforce a maximum entry cap with LRU eviction.
  3. In `set_with_tags`, check if `k` already exists in `state.entries`; if so, remove `k` from any old tags no longer present in `tags_set`.

---

### M5 `Cache::remember` lacks stampede (thundering herd) protection on cache miss

- **Where:** `crates/core/src/cache/typed.rs:127–161`
- **What it says:**
  ```rust
  pub async fn remember_with_tags<T, F, Fut>(
      &self,
      key: &str,
      tags: &[&str],
      ttl: Option<Duration>,
      f: F,
  ) -> Result<T, CacheError>
  where
      T: Serialize + DeserializeOwned,
      F: FnOnce() -> Fut,
      Fut: std::future::Future<Output = Result<T, CacheError>>,
  {
      if let Some(cached) = self.get_json::<T>(key).await? {
          return Ok(cached);
      }

      let fresh = f().await?;
      self.set_json_with_tags(key, &fresh, tags, ttl).await?;
      Ok(fresh)
  }
  ```
- **Why it's a problem:** In enterprise systems with high concurrent traffic (e.g. hundreds of concurrent requests for a product page or account dashboard), when a cached item expires or is invalidated:
  1. All concurrent requests invoke `get_json::<T>(key)` simultaneously and observe a cache miss (`None`).
  2. Every concurrent caller proceeds to execute `f().await` simultaneously.
  3. The underlying database or upstream microservice is immediately inundated with hundreds of identical, heavy queries (**cache stampede / dogpile effect**), frequently triggering connection pool exhaustion, database query spikes, and service degradation.
  4. All callers then race to write the identical computed value back to `set_json_with_tags`.
- **Suggested fix:** Implement single-flight request coalescing (similar to Go's `singleflight` or Tokio-based lock striping). When a cache miss occurs, only the first caller executes `f().await` while concurrent callers for the same key await the completion of the in-flight future and share the resulting value.

---

### M6 `LeaderElector` lacks monotonic fencing tokens or epoch tracking for split-brain protection

- **Where:** `crates/core/src/coordination/leader.rs:13–35, 38–41, 73–100`
- **What it says:**
  ```rust
  pub trait LeaderElector: Debug + Send + Sync + 'static {
      fn try_acquire<'a>(
          &'a self,
          role: &'a str,
          candidate_id: &'a str,
          ttl: Duration,
      ) -> CoordinationFuture<'a, bool>;
  ...
  #[derive(Debug, Clone)]
  struct ActiveLeader {
      candidate_id: String,
      expires_at: Instant,
  }
  ```
- **Why it's a problem:** Unlike `DistributedLock` (`crates/core/src/coordination/lock.rs:21–28`), which returns a `LockGuard` containing a monotonically increasing `fencing_token: u64`, `LeaderElector::try_acquire` returns only a bare `bool`.
  In distributed systems with active-passive failover:
  1. If Leader A experiences a temporary network pause or prolonged garbage collection/task stall, its lease expires in the election registry.
  2. Leader B is elected as the new leader.
  3. Leader A recovers from its pause. Until Leader A attempts to renew its lease, it believes it is still the leader.
  4. Without a monotonic term, epoch, or fencing token returned on election and passed to downstream storage systems or outbox relays, both Leader A and Leader B can perform conflicting mutations (**split-brain scenario**), corrupting persistent data.
- **Suggested fix:** Update `LeaderElector` to issue an `ElectionTerm` or `LeaderGuard` struct containing a monotonically increasing `term: u64` or `fencing_token: u64` (e.g. `try_acquire(...) -> CoordinationFuture<'a, Option<LeaderGuard>>`), matching the pattern established by `MemoryDistributedLock`. Downstream systems can then enforce monotonic fencing checks.

---

### m1 `MetricsService` leaks `in_flight` gauge on cancelled request futures due to missing RAII drop guard

- **Where:** `crates/core/src/metrics/layer.rs:69–75`
- **What it says:**
  ```rust
  registry.inc_in_flight();

  Box::pin(async move {
      let response = inner.call(req).await;
      let duration = start.elapsed();
      registry.dec_in_flight();
  ```
- **Why it's a problem:** If a client drops the TCP connection while `inner.call(req).await` is pending, or if an outer middleware (such as `TimeoutLayer`) aborts the request future when a deadline expires, Tokio drops the pinned future before completion. Because `registry.dec_in_flight()` is placed sequentially after `await` without an RAII drop guard, it is skipped. Over time, `http_requests_in_flight` leaks positive increments, showing hundreds or thousands of phantom active requests on Prometheus dashboards when the server is idle.
- **Suggested fix:** Wrap the in-flight increment in an RAII guard struct whose `Drop` implementation calls `registry.dec_in_flight()` unconditionally when the future terminates.

---

### m2 `OutboxRelay` lacks retry backoff, rapidly exhausting retry thresholds during downstream outages

- **Where:** `crates/core/src/outbox/relay.rs:59–78` and `crates/core/src/outbox/repository.rs:102–120`
- **What it says:**
  ```rust
  if let Some(msg) = messages.get_mut(&id_str) {
      msg.retry_count += 1;
      msg.last_error = Some(error);
      if msg.retry_count >= 5 {
          msg.status = OutboxStatus::Failed;
      }
  }
  ```
- **Why it's a problem:** When an external message broker or event bus is unreachable, `poll_and_dispatch` runs every `interval` (e.g. 50ms). Because `OutboxMessage` does not store a `next_retry_at` timestamp, `fetch_pending` immediately refetches the failing message on every single tick. The message burns through all 5 retries in approximately 250 milliseconds and transitions to `OutboxStatus::Failed`, defeating the purpose of retry resilience for transient network outages.
- **Suggested fix:** Add `next_retry_at: Option<u64>` to `OutboxMessage`. In `mark_failed`, compute exponential backoff (e.g. $2^{\text{retry\_count}}$ seconds with jitter). Update `fetch_pending` to only retrieve messages where `now >= next_retry_at`.

---

### m3 `MemoryOutboxRepository::fetch_pending` dispatches messages in random UUID hash order rather than chronological sequence

- **Where:** `crates/core/src/outbox/repository.rs:64–83`
- **What it says:**
  ```rust
  let pending = messages
      .values()
      .filter(|m| m.status == OutboxStatus::Pending)
      .take(limit)
      .cloned()
      .collect();
  ```
- **Why it's a problem:** `messages` is a `BTreeMap<String, OutboxMessage>` keyed by `msg.id`, which is generated via `Uuid::new_v4().to_string()` (`crates/core/src/outbox/message.rs:59`). Iterating `messages.values()` traverses messages in alphabetical order of their random UUID strings, completely ignoring the `created_at` timestamp. Messages are dispatched out-of-order, violating causal and temporal event delivery guarantees for downstream consumers.
- **Suggested fix:** Sort pending messages by `created_at` (or maintain an ordered index by `(created_at, id)` or monotonic sequence number) when returning batches from `fetch_pending`.

---

### m4 `TenantScopedCache` and `Cache` omit tag prefix namespacing and tenant memory quotas

- **Where:** `crates/core/src/tenant/cache.rs:23–66` and `crates/core/src/cache/typed.rs:61–68, 116–124, 182–187`
- **What it says:**
  In `crates/core/src/cache/typed.rs`:
  ```rust
  fn format_key(&self, key: &str) -> String {
      if let Some(prefix) = &self.config.prefix {
          format!("{prefix}{key}")
      } else {
          key.to_string()
      }
  }
  ...
  self.backend
      .set_with_tags(&full_key, bytes, tags, eff_ttl)
      .await
  ...
  self.backend.invalidate_tag(tag).await
  ```
  and `TenantScopedCache`:
  ```rust
  pub async fn invalidate_tenant(&self, tenant_id: &str) -> Result<usize, CacheError> {
      let tag = Self::tenant_tag(tenant_id);
      self.cache.invalidate_tag(&tag).await
  }
  ```
- **Why it's a problem:**
  1. **Tag namespace leakage:** While `Cache::format_key` applies `config.prefix` to cache keys, it passes `tags` to `backend.set_with_tags` and `backend.invalidate_tag` completely un-prefixed. If two tenant applications share a cache backend with different key prefixes, calling `invalidate_tag("users")` in one tenant will wipe out cached entries tagged `"users"` across all tenants.
  2. **Noisy-neighbor resource exhaustion:** `TenantScopedCache` enforces no per-tenant key limits or memory quotas. A single abusive or misconfigured tenant can insert millions of keys, exhausting cache memory and starving all other tenants sharing the system.
- **Suggested fix:** Prepend `config.prefix` to tags in `Cache::set_json_with_tags` and `Cache::invalidate_tag`. In `TenantScopedCache`, add configurable per-tenant quota bounds (e.g. `max_keys_per_tenant`).

---

## Positive notes

- **Monotonic distributed lock fencing:** `MemoryDistributedLock` (`crates/core/src/coordination/lock.rs:91–112`) issues strictly monotonic fencing tokens (`fencing_token: u64`) on every lock acquisition and lease expiration, providing correct fencing primitives to protect storage engines against split-brain writes.
- **RAII concurrency permits:** `ConcurrencyLimiter` (`crates/core/src/resilience/concurrency.rs:55–63`) uses an RAII `ConcurrencyPermit` struct whose `Drop` implementation unconditionally decrements the active concurrency counter, preventing permit leaks on request completion or failure.
- **Non-blocking request timeouts:** `TimeoutLayer` (`crates/core/src/resilience/timeout.rs:66–73`) wraps request execution in `tokio::time::timeout` and returns RFC 9457 Problem Details (`504 GATEWAY_TIMEOUT`) with structured JSON error details without blocking the Tokio reactor.
- **Exclusion of `/metrics` scraping from telemetry counters:** `MetricsService` (`crates/core/src/metrics/layer.rs:59–62`) explicitly filters out requests to `/metrics`, avoiding self-referential counter inflation during Prometheus scraping sweeps.
- **Standard Prometheus exposition formatting:** `MetricsRegistry::render_prometheus` (`crates/core/src/metrics/registry.rs:98–152`) produces fully valid Prometheus exposition format output, correctly partitioned by HTTP method and status code with `# HELP` and `# TYPE` headers.
- **Seamless cache health check integration:** `Cache::health_check` (`crates/core/src/cache/typed.rs:203–209`) implements Rume's standard `HealthCheck` trait, allowing cache connectivity and closed-state diagnostics to be reported automatically via `/health/ready`.
- **Sliding-window rate limit headers:** `RateLimitService` (`crates/core/src/resilience/rate_limiter.rs:161–182`) injects standard rate-limiting headers (`x-ratelimit-limit`, `x-ratelimit-remaining`, and `Retry-After`) into both accepted responses and 429 Problem Details rejections.

---

## Verdict

**Not ready to ship as-is without a fix pass.**

While the architectural abstractions for enterprise reliability are well-structured, the implementation contains **2 Critical findings** and **6 Major findings** that will cause memory exhaustion, duplicate message delivery, and operational failures under production conditions:

1. **C1** — Replace unbounded `Vec<f64>` latency sample vectors in `MetricsRegistry` with atomic counters, sum accumulators, and fixed histogram buckets to prevent Linux OOM process kills and scraping stalls.
2. **C2** — Prevent duplicate message publication across HA replicas by implementing row claiming/locking (`FOR UPDATE SKIP LOCKED` or `InFlight` status) in `OutboxRelay` and guarding relays with leader election.
3. **M1** — Bound `JobWorker` concurrency with a semaphore/worker pool to prevent unbounded Tokio task spawning during queue spikes.
4. **M2** — Wire `HeartbeatRegistry` and abandoned job recovery into `JobWorker` so worker node failures do not permanently orphan in-flight jobs.
5. **M3** — Clean up inactive client keys in `RateLimiter` and eliminate single-lock contention across concurrent requests.
6. **M4** — Add TTL eviction and capacity bounds to `MemoryCacheBackend` and clean up old tag mappings on key updates.
7. **M5** — Add single-flight stampede protection to `Cache::remember` to protect downstream databases from thundering-herd spikes on cache miss.
8. **M6** — Issue monotonic term/epoch fencing tokens in `LeaderElector` to prevent split-brain mutations during active-passive failover.

Remediating these findings will bring Rume's operational reliability, multi-worker coordination, and high-availability guarantees up to enterprise standards.
