# TODO — Rume Architecture & Security Review (v05) Remediation Plan

Tracking remediation for findings in the 2026-11-08 comprehensive architecture & security audit report across all four subsystem groups.

---

## Group 1 — Job Queue & Worker Reliability (P0 / P1)

- [x] **Jobs R1 — Fail closed on idempotency lock acquisition error**
      `crates/core/src/jobs/worker.rs:653`: When `store.try_acquire_with_token` returns `Err(err)`, worker logs error and executes handler anyway. Fail closed by nacking with retry backoff; do not execute handler.
- [x] **Jobs R2 — Break `JobQueue` recursive default enqueue methods**
      `crates/core/src/jobs/queue.rs:20–39`: `enqueue_delayed` and `enqueue_delayed_with_trace` mutually delegate, risking stack overflow in third-party backends. Make `enqueue_delayed_with_trace` mandatory without circular default. Add custom backend test.
- [x] **Jobs R3 — Enforce idempotency store fencing contract & check Redis Lua return values**
      `crates/core/src/jobs/reliability.rs:35–58` & `crates/core/src/jobs/redis.rs:337, 388`: Trait default methods discard owner tokens, and Redis store discards integer match results. Fail if token does not match owner.
- [x] **Jobs R4 — Renew idempotency lock during long-running worker heartbeats**
      `crates/core/src/jobs/worker.rs:671`: Worker heartbeat renews queue lease but allows 300s idempotency lock to expire during long-running jobs. Renew idempotency lock in worker heartbeat task.
- [x] **Jobs R5 — Worker supervision aborts remaining tasks on timeout**
      `crates/core/src/jobs/worker.rs:196`: `WorkerHandle::shutdown()` consumes `self` and returns `false` on timeout without aborting orphaned tasks. Add `try_shutdown(&mut self)` and abort remaining tasks on timeout in consuming `shutdown()`.

---

## Group 2 — Resilience, Security & Multi-Tenant Isolation (P0 / P1 / P2)

- [x] **Resilience R1 — Rate limiting client identity derivation from socket peer**
      `crates/core/src/resilience/rate_limiter.rs:247–270`: Derives key from unauthenticated `x-tenant-id` header or untrusted `x-forwarded-for`. Remove `x-tenant-id` header fallback; derive client IP from socket peer (`ConnectInfo`) or validated trusted proxy.
- [x] **Resilience R2 — Bound in-memory rate limiter key cardinality & inline eviction**
      `crates/core/src/resilience/rate_limiter.rs:44–138`: Memory grows unbounded until background `prune_expired()`. Bound shard entry capacity and prune expired entries inline on write.
- [x] **Security R1 — CSRF exemption path-segment boundary matching**
      `crates/core/src/security/csrf.rs:333`: `path.starts_with(exempt)` matches `/api/webhooks-admin` for `/api/webhooks`. Require path-segment boundary or exact match.
- [x] **Security R2 — Secure proxy defaults in `SecurityHeadersConfig`**
      `crates/core/src/security/headers.rs:62, 193`: `trust_proxy_headers` defaults to `true` and empty `trusted_proxies` trusts all clients. Default `trust_proxy_headers` to `false` and require non-empty `trusted_proxies`.
- [x] **Tenant R1 — In-memory tenant cache quota tracks unique keys**
      `crates/core/src/tenant/quota.rs:53–112` & `crates/core/src/tenant/cache.rs:103–133`: Count increments on every write rather than unique keys, and write failures remove quota for existing keys. Track unique key sets and ensure idempotent acquisition.

---

## Group 3 — Persistence & Migration Integrity (P1 / P2)

- [x] **Persistence R1 — Reject empty/blank migration checksums**
      `crates/core/src/persistence/migration.rs:229, 303, 336, 384, 513`: `!actual.is_empty() && actual != expected` allows blank checksums to bypass verification. Fail closed on empty/mismatched checksums.
- [x] **Persistence R2 — Dialect-specific distributed migration locking across all backends**
      `crates/core/src/persistence/migration.rs:325–400`: Only PostgreSQL acquires advisory locks. Add MySQL `GET_LOCK('rume_migrations', 60)` / `RELEASE_LOCK` and SQLite exclusive transaction locking.

---

## Group 4 — Middleware, Observability & Outbox Lifecycles (P1 / P2)

- [x] **Observability R1 — Tower readiness contract preservation across all middleware**
      `crates/core/src/app/observability.rs`, `rate_limiter.rs`, `security/headers.rs`, `security/csrf.rs`, etc.: Polling `self.inner.poll_ready` and calling a fresh clone breaks readiness. Take ready instance via `std::mem::replace(&mut self.inner, clone)`.
- [x] **Observability R2 — Preserve oversized/streaming response bodies in Problem Details inspection**
      `crates/core/src/app/observability.rs:506–510`: `to_bytes()` failure replaces original response with `Body::empty()`. Preserve original body if inspection cannot complete safely.
- [x] **Outbox R1 — Enforce drain timeout around outbox message dispatch**
      `crates/core/src/outbox/relay.rs:529–555`: `try_drain` checks elapsed before dispatch, but awaits dispatcher indefinitely. Wrap dispatch in remaining drain deadline. Add test with stalled dispatcher.
