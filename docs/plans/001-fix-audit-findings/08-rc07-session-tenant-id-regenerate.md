# 08-rc07-session-tenant-id-regenerate.md

## Goal

Fix RC-07 session tenancy and regeneration defects:

1. `crates/core/src/auth/session.rs`: `Session` has no `tenant_id` field, so sessions cannot be bound to a tenant.
2. `crates/core/src/auth/schemes.rs`: `SessionScheme::authenticate` builds an `AuthUser` from the session but never propagates `tenant_id` to the user (cross-tenant attribution lost). Note `AuthUser` already has `tenant_id`/`with_tenant_id` (identity.rs).
3. `crates/core/src/auth/redis.rs`: `RedisSessionStore::regenerate_id` Lua script interpolates the raw `old_id` (a UUID with hyphens) into a Lua *pattern* (`'"id":"' .. old_id .. '"'`); Lua treats `-` as a non-greedy pattern operator, so UUIDs containing hyphens fail to match/replace — session regeneration silently breaks for UUID session IDs.
4. `crates/core/src/auth/session.rs`: `InMemorySessionStore` uses the trait's default `regenerate_id` (load → save → destroy, non-atomic, read between write and delete) which is vulnerable to session-fixation races.

Fix root cause:
- Add `tenant_id: Option<String>` to `Session` (constructor, `is_expired` unaffected, builders).
- In `SessionScheme`, propagate `session.tenant_id` into `user.with_tenant_id(...)`.
- In `redis.rs`, escape the Lua pattern magic characters in `old_id`/`new_id` (or use a literal replacement approach) so hyphenated UUIDs regenerate correctly.
- Override `regenerate_id` in `InMemorySessionStore` to be atomic: hold the write lock across load → save → destroy so no interleaved destroy can resurrect the old id.

## Files touched

- `crates/core/src/auth/session.rs` — `Session` struct + `Session::new`, `InMemorySessionStore::regenerate_id`.
- `crates/core/src/auth/schemes.rs` — `SessionScheme::authenticate` (propagate tenant_id).
- `crates/core/src/auth/redis.rs` — `RedisSessionStore::regenerate_id` Lua pattern escaping.

## Constraints honoured

- Root cause, not symptom: tenant binding is stored on the session and propagated, not inferred later.
- No new dependencies, no `unsafe`.
- `regenerate_id` atomicity: old id is destroyed only after the new id is saved, all under one write lock.
- Record `Why:` comments for the Lua escaping and the atomic regenerate_id.

## Done when (checkable by a command)

- `cargo test -p rume-core auth` passes.
- `cargo clippy -p rume-core --all-targets` and `cargo fmt --all --check` clean.
- `Session` has a `tenant_id` field; `SessionScheme` sets `user.tenant_id` from the session.
- `InMemorySessionStore::regenerate_id` is overridden (not the trait default).

## Verification (fails-before test idea)

Tests:
1. **tenant_id propagation:** create a `Session` with `tenant_id = Some("t1")`, load via `SessionScheme`, assert the resulting `AuthUser.tenant_id == Some("t1")`. Before the fix it is `None` → fails.
2. **Lua hyphen bug:** regenerate a session whose id contains hyphens (a real UUID) through `RedisSessionStore` against a fake/lua-asserting harness, assert the new id replaces the old and the old is removed. Before the fix the Lua pattern fails to match the hyphenated id → fails.
3. **Atomic regenerate:** concurrent regenerate_id + destroy on `InMemorySessionStore`; assert exactly one session exists with the new id and the old id is gone (no resurrection). Before the fix the non-atomic default can resurrect the old id → fails.
