# Task 04 — Instrumented Outbound HTTP Client (`RumeClient`)

## Goal

Implement `RumeClient` and `RumeClientBuilder` in `rume-http::client` wrapping `reqwest::Client` with W3C `traceparent`/`tracestate` and `X-Request-ID` propagation, `CircuitBreaker` integration, configurable exponential backoff + jitter retries, per-attempt and total timeouts, and `ServiceDiscovery` (`DnsSrvDiscovery` behind `dns-srv`).

## Files touched

- `crates/http/src/client/{mod.rs, discovery.rs}`
- `crates/http/src/lib.rs`

## Deliverable

1. `RumeClient` and `RumeClientBuilder` with convenience methods (`get`, `post`, `put`, `delete`, `patch`, `request`, `execute`) returning `Result<reqwest::Response, OutboundHttpError>`.
2. Automatic header injection: W3C `traceparent` / `tracestate` from `rume_observability::TraceContext::current()` and `X-Request-ID` (propagated or generated via `RequestId::generate()`).
3. `CircuitBreaker` integration using `acquire_permit()` -> `TrialPermit::complete_success()` / `complete_failure()` on 5xx or transport errors, rejecting calls with `OutboundHttpError::CircuitOpen` when open.
4. `RetryConfig` with exponential backoff + full jitter (`rand`), retrying only idempotent methods (`GET`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`) by default unless explicitly opted in, plus per-attempt and total request timeouts.
5. `ServiceDiscovery` trait + `StaticDiscovery` + `#[cfg(feature = "dns-srv")] DnsSrvDiscovery` using `hickory-resolver = "=0.26.4"` to resolve SRV records (ordered by priority/weight).

## Constraints honoured

- **Resilience invariants**: `crates/http/src/resilience/circuit_breaker.rs` (`TrialPermit` drop-guard semantics).
- **Observability invariants**: `crates/observability/src/trace_context.rs` (`TRACEPARENT`, `TRACESTATE`, `TraceContext::current`) and `crates/foundation/src/telemetry.rs` (`X_REQUEST_ID`, `RequestId`).
- **TDD (`rules/50-agent-conduct.md`)**: Write hermetic tests using local `axum` + `tokio::net::TcpListener` servers first, confirm fails-before, then implement.

## Named tests (bodies omitted)

- `rume_client_injects_traceparent_tracestate_and_request_id` — verifies outbound request carries active `TraceContext` and `X-Request-ID`.
- `rume_client_retries_idempotent_get_with_backoff_and_succeeds` — server returns two 503s then 200; verifies 3 attempts and success.
- `rume_client_does_not_retry_post_unless_opted_in` — server returns 503 on POST; verifies 1 attempt by default, retries when `.retry_non_idempotent(true)` is set.
- `rume_client_trips_circuit_breaker_and_recovers` — consecutive 500s open the circuit, subsequent call fails fast with `CircuitOpen`, and after `reset_timeout` half-open probe closes it.
- `rume_client_enforces_total_timeout` — slow server exceeds total timeout and returns `OutboundHttpError::Timeout`.
- `static_discovery_and_srv_selection_resolve_targets` — verifies `ServiceDiscovery` target resolution and SRV priority/weight ordering logic.

## Verification

```bash
cargo test -j 2 -p rume-http --lib client --all-features
cargo clippy -j 2 -p rume-http --all-targets --all-features -- -D warnings
```

## Commit

Only when the user asks:

```text
feat(http): add instrumented RumeClient with tracing, circuit breaker, retries, and discovery
```

## Done-when

- [ ] `RumeClient` injects `traceparent`, `tracestate`, and `X-Request-ID`.
- [ ] `CircuitBreaker`, idempotent-by-default retries with jitter, and timeouts pass tests.
- [ ] `ServiceDiscovery` and `#[cfg(feature = "dns-srv")] DnsSrvDiscovery` compile and pass tests.
- [ ] Zero clippy warnings in `rume-http`.
