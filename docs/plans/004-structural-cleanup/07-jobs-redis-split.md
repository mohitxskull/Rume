# Task 07 — Split `crates/messaging/src/jobs/redis.rs`

## Goal

Split `crates/messaging/src/jobs/redis.rs` (490 lines) into `crates/messaging/src/jobs/redis/{mod,heartbeat,idempotency}.rs`
— behavior-preserving; `cargo build` + `cargo test -p rume-messaging --features redis` green.

## Files touched

- `crates/messaging/src/jobs/redis.rs` — deleted; replaced by:
- `crates/messaging/src/jobs/redis/mod.rs` — module docs (redis.rs:1); `mod heartbeat; mod idempotency;`
  + re-exports `pub use heartbeat::RedisHeartbeatRegistry; pub use idempotency::RedisIdempotencyStore;`
  (so `jobs/mod.rs:15` `pub use self::redis::{RedisHeartbeatRegistry, RedisIdempotencyStore};`
  resolves, and `rume::jobs::RedisHeartbeatRegistry`/`RedisIdempotencyStore` are unchanged).
- `crates/messaging/src/jobs/redis/heartbeat.rs` — `RedisHeartbeatRegistry` (18-156) +
  `impl HeartbeatRegistry for RedisHeartbeatRegistry` (`record_heartbeat` 56-92, `is_alive` 94-123,
  `reap_dead_workers` 125-156).
- `crates/messaging/src/jobs/redis/idempotency.rs` — `RedisIdempotencyStore` (163-207) + `impl
  IdempotencyStore` (`try_acquire`/`try_acquire_with_token` 218-290, `complete`/`complete_with_token`
  296-358, `release`/`release_with_token` 360-413, `extend_lock_with_token` 415-469), `scoped_key` (200-202),
  `key` (204-206), `hex_encode` (472-478), `hex_decode` (480-490).

## Deliverable

`rume::jobs::{RedisHeartbeatRegistry, RedisIdempotencyStore}` resolve (module paths via
`jobs/mod.rs:15`). Consumers verified: `crates/core/tests/containers/redis.rs:22-23,463,498,543`
and `crates/core/tests/containers/job_reliability.rs:8,44,95,142,213,272` — all unchanged.

## Constraints honoured

- `jobs/mod.rs:15` unchanged.
- No behavior change (Redis Lua scripts move verbatim); `cargo build` + `cargo test -p rume-messaging
  --features redis` green.

## Named tests — none (`crates/messaging/src/jobs/redis.rs` has no tests)

None — `redis.rs` has no `#[test]`/`mod tests` (verified: no test module in the file).

## Verification

```
cargo build
cargo test -p rume-messaging --features redis
```

## Commit

Only when the user asks:

```
refactor(messaging/jobs): split jobs/redis.rs into jobs/redis/ heartbeat and idempotency modules
```

## Done-when

- [ ] `crates/messaging/src/jobs/redis/` contains `mod.rs`, `heartbeat.rs`, `idempotency.rs`;
  `crates/messaging/src/jobs/redis.rs` deleted.
- [ ] `rume::jobs::{RedisHeartbeatRegistry, RedisIdempotencyStore}` resolve.
- [ ] `cargo build` + `cargo test -p rume-messaging --features redis` green.
