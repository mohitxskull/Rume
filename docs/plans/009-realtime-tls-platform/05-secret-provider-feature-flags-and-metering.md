# Task 05 — Dynamic Secret Provider, Feature Flags & Usage Metering

## Goal

Implement the first three platform building blocks: dynamic `SecretProvider` (`EnvSecretProvider`, `CachedSecretProvider`, `Vault`/`Kms` hook) in `rume-foundation`, `FeatureFlagStore` (`InMemoryFeatureFlags`, `SqlFeatureFlags`, tenant/user targeting, deterministic SHA-256 rollout) in `rume-security`, and `UsageMeter` (`InMemoryUsageMeter`, `SqlUsageMeter`, `BillingSink`) in `rume-security`.

## Files touched

- `crates/foundation/src/env/secret_provider.rs`, `crates/foundation/src/env/mod.rs`
- `crates/security/src/flags/mod.rs` (and submodules `store.rs`, `sql.rs`, `eval.rs`)
- `crates/security/src/metering/mod.rs` (and submodules `meter.rs`, `sql.rs`)
- `crates/security/src/lib.rs`
- `crates/security/Cargo.toml` (already wired in Task 01 with optional `sqlx` + `sqlite`/`postgres`/`mysql` features)

## Deliverable

- **Dynamic Secret Provider (`rume-foundation::env::secret_provider`)**:
  - `SecretProvider` trait (`get_secret(key) -> SecretString`, `rotate(key)`).
  - `EnvSecretProvider` reading process environment variables (or a custom map for hermetic tests).
  - `CachedSecretProvider` wrapping any `Arc<dyn SecretProvider>` with configurable TTL cache, `invalidate(key)`, and `on_rotation` callback hook.
  - `ExternalSecretBackend` / `VaultSecretProvider` / `KmsSecretProvider` adapter hooks (trait-backed so callers plug in Vault/KMS HTTP/SDK clients without hard-coding a vendor SDK).
- **Feature Flags (`rume-security::flags`)**:
  - `FeatureFlag`, `TargetingRule` (`Tenant(String)`, `User(String)`), `RolloutPercentage(u8)`.
  - `FeatureFlagStore` trait (`upsert_flag`, `get_flag`, `list_flags`, `delete_flag`, `evaluate(key, ctx)`).
  - `InMemoryFeatureFlags` and `SqlFeatureFlags` (SQLite/Postgres/MySQL via `rume_data::sql`).
  - Deterministic percentage bucketing via `sha2::Sha256` over `"{flag_key}:{subject_id}"` modulo `100`.
- **Usage Metering (`rume-security::metering`)**:
  - `UsageRecord` (`tenant_id`, `metric`, `quantity`, `recorded_at`), `UsageMeter` trait (`record_usage`, `get_usage`, `flush`).
  - `InMemoryUsageMeter` and `SqlUsageMeter` (SQLite/Postgres/MySQL via `rume_data::sql`).
  - `BillingSink` trait (`export_batch(&[UsageRecord])`) invoked on `flush()`.

## Constraints honoured

- `crates/foundation/src/env/secrets.rs` (`redact_value`, `secrecy::SecretString` — never log secret values).
- `crates/data/src/sql.rs` (central SQL constants for `rume_feature_flags` and `rume_usage_counters`).
- `crates/security/src/tenant/model.rs` (`TenantId` isolation).
- `#![forbid(unsafe_code)]`, `rules/80-quality-bar.md`.

## Named tests (bodies omitted)

- `rume_foundation::env::secret_provider::tests::cached_secret_provider_respects_ttl_and_invokes_rotation_callback` — verifies cached hit within TTL, reload after TTL expiry/invalidation, and rotation callback invocation without leaking secret values in `Debug`.
- `rume_security::flags::tests::feature_flag_targeting_and_sha256_rollout_are_deterministic` — verifies tenant/user allow-list overrides and that SHA-256 percentage bucketing returns identical results across repeated evaluations for the same `(flag, subject)`.
- `rume_security::flags::tests::sql_feature_flags_roundtrip_on_sqlite` — (under `sqlite`) upserts, evaluates, and deletes flags in `SqlFeatureFlags`.
- `rume_security::metering::tests::usage_meter_aggregates_per_tenant_and_flushes_to_billing_sink` — records usage across two tenants, asserts per-tenant totals, flushes to a mock `BillingSink`, and verifies exported batch + reset/checkpoint semantics.

## Verification

```bash
cargo test -j 2 -p rume-foundation -p rume-security --all-features
```
Expected: all `secret_provider`, `flags`, and `metering` tests pass with zero warnings.

## Commit

Only when the user asks:
```text
feat(foundation,security): add SecretProvider, FeatureFlagStore, and UsageMeter
```

## Done-when

- [ ] `SecretProvider`, `EnvSecretProvider`, `CachedSecretProvider`, and Vault/KMS hook traits work with TTL caching and rotation callbacks.
- [ ] `FeatureFlagStore` (`InMemoryFeatureFlags`, `SqlFeatureFlags`) evaluates tenant/user rules and deterministic SHA-256 percentage rollouts.
- [ ] `UsageMeter` (`InMemoryUsageMeter`, `SqlUsageMeter`) and `BillingSink` aggregate and flush tenant-scoped counters.
