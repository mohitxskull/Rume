# Task 08 — Split `crates/data/src/coordination/redis.rs`

## Goal

Split `crates/data/src/coordination/redis.rs` (464 lines) into
`crates/data/src/coordination/redis/{mod,lock,leader}.rs` — behavior-preserving;
`cargo build` + `cargo test -p rume-data` green.

## Files touched

- `crates/data/src/coordination/redis.rs` — deleted; replaced by:
- `crates/data/src/coordination/redis/mod.rs` — `mod leader; mod lock;` + re-exports
  `pub use leader::RedisLeaderElector; pub use lock::RedisDistributedLock;` (so
  `coordination/mod.rs:9` `pub use self::redis::{RedisDistributedLock, RedisLeaderElector};`
  resolves).
- `crates/data/src/coordination/redis/lock.rs` — `RedisDistributedLock` (23-66) +
  `impl DistributedLock for RedisDistributedLock` (`acquire` 69-124, `renew` 126-175, `release` 177-209).
- `crates/data/src/coordination/redis/leader.rs` — `RedisLeaderElector` (218-261) +
  `impl LeaderElector for RedisLeaderElector` (`try_acquire` 264-321, `renew` 323-390,
  `get_leader` 392-427, `resign` 428-463).

## Deliverable

`rume::coordination::{RedisDistributedLock, RedisLeaderElector}` resolve via
`coordination/mod.rs:9` (re-exports through `redis/mod.rs`). No behavior change (Lua scripts move
verbatim).

## Constraints honoured

- `crates/data/src/coordination/mod.rs:9` unchanged.
- No behavior change; `cargo build` + `cargo test -p rume-data` green.

## Named tests — none (`crates/data/src/coordination/redis.rs` has no tests)

None — `crates/data/src/coordination/redis.rs` has no tests.

## Verification

```
cargo build
cargo test -p rume-data
```

## Commit

Only when the user asks:

```
refactor(data): split coordination/redis.rs into coordination/redis/ lock and leader modules
```

## Done-when

- [ ] `crates/data/src/coordination/redis/` contains `mod.rs`, `lock.rs`, `leader.rs`;
  `crates/data/src/coordination/redis.rs` deleted.
- [ ] `rume::coordination::{RedisDistributedLock, RedisLeaderElector}` resolve.
- [ ] `cargo build` + `cargo test -p rume-data` green.
