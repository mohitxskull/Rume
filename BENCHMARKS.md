# Rume Benchmarks & Load Testing

This document summarizes Rume's reproducible benchmark and load-testing suites so contributors and evaluators can measure exact abstraction overhead over raw Axum and verify production-like workloads locally.

## 1. Hot-Path Criterion Micro-Benchmarks (`crates/core/benches/hot_path.rs`)

Measures in-process overhead of Rume's HTTP middleware, resilience primitives, and caching layers against bare `axum::Router`.

### Reproduce

```bash
cargo bench -p rume --bench hot_path --all-features
```

### Measured Baseline

- **Hardware:** 11th Gen Intel(R) Core(TM) i5-1135G7 @ 2.40GHz (8 logical cores), Linux x86_64
- **Toolchain:** `rustc 1.96.1` (`--bench` optimized release profile)
- **Command:** `cargo bench -p rume --bench hot_path --all-features -- --warm-up-time 1 --measurement-time 2 --sample-size 20`

| Benchmark | Median Time | Notes |
|---|---:|---|
| `hot_path/raw_axum_router` | **919 ns** | Bare `axum::Router::new().route("/ping", ...)` (`oneshot`) |
| `hot_path/router_plus_observability_layer` | **4.20 µs** | `+3.28 µs` delta for `ObservabilityLayer` (W3C `traceparent` extraction, child span, `RequestId` injection) |
| `hot_path/circuit_breaker_try_acquire_closed` | **48.2 ns** | `CircuitBreaker::try_acquire` + `TrialPermit::complete_success` (`parking_lot::RwLock`, closed fast path) |
| `hot_path/rate_limiter_in_memory_check` | **304 ns** | Sharded in-memory sliding window (`parking_lot::RwLock`) with 32 seeded timestamps in window |
| `hot_path/tenant_scoped_cache_get_hit` | **409 ns** | `TenantScopedCache::get` hit (tenant prefixing + in-memory lookup + deserialization) |
| `hot_path/tenant_scoped_cache_set_overwrite` | **1.68 µs** | `TenantScopedCache::set` overwrite (tenant read lock + per-key `KeyLockGuard` + quota reservation + backend write) |
| `hot_path/singleflight_cache_64_concurrent_tasks` | **54.3 µs** | 64 concurrent Tokio tasks calling `Cache::remember` on the same cold key (~848 ns/task, single upstream compute) |

## 2. Multi-Instance System Baseline (`workbench/multi-instance/benches/baseline.rs`)

Measures real TCP HTTP latency (Raw Axum vs. Rume Pipeline across concurrency tiers `[1, 8, 32, 64, 128]`), SQLite & PostgreSQL pool checkout, competing transactional outbox relays (`FOR UPDATE SKIP LOCKED`), and competing job workers with explicit SLA budgets.

### Reproduce (requires Docker)

```bash
cargo bench -p workbench --bench baseline
```

## 3. Goose Realistic 80/20 Load Testing (`benchmarks/load/`)

Standalone Rust [Goose](https://book.goose.rs) load harness (excluded from the root workspace so it adds zero dependencies to `rume`) targeting the live `workbench/multi-instance` Docker Compose stack (PostgreSQL + Redis + 2 Rume instances).

### Reproduce

1. Build and start the multi-instance stack:

```bash
cargo build --release -p workbench --bin workbench
docker compose -f workbench/multi-instance/docker-compose.yml up -d --build workbench-1
docker compose -f workbench/multi-instance/docker-compose.yml up -d
```

2. Run Goose (public health + organization read mix):

```bash
cargo run --release --manifest-path benchmarks/load/Cargo.toml -- \
  --host http://127.0.0.1:8081 \
  --users 64 \
  --hatch-rate 16 \
  --run-time 60s
```

3. For the full authenticated 80/20 tenant read/write + transactional outbox mix (`GET /api/v1/projects`, `GET /api/v1/projects/{id}/issues`, `POST /api/v1/projects/{id}/issues`), set:

```bash
RUME_BENCH_TENANT_ID="<tenant-id>" \
RUME_BENCH_BEARER_TOKEN="<bearer-token>" \
RUME_BENCH_PROJECT_ID="<project-id>" \
cargo run --release --manifest-path benchmarks/load/Cargo.toml -- \
  --host http://127.0.0.1:8081 \
  --users 64 \
  --hatch-rate 16 \
  --run-time 5m \
  --report-file benchmarks/load/report.html
```

4. Tear down when finished:

```bash
docker compose -f workbench/multi-instance/docker-compose.yml down --volumes --remove-orphans
```
