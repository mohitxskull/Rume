# Task 03 — Leader-Elected Cron Scheduler

## Goal

Implement `CronScheduler` and `CronSchedulerHandle` in `rume-messaging::cron` combining `rume_data::coordination::LeaderElector` and `croner = "=5.0.1"` so recurring cron jobs execute once per cluster, with timezone offset support, `MissedTickPolicy`, and graceful shutdown.

## Files touched

- `crates/messaging/src/cron/{mod.rs, scheduler.rs, spec.rs}`
- `crates/messaging/src/lib.rs`

## Deliverable

1. `CronJobSpec` supporting 5-field and 6-field cron expressions validated via `croner::Cron::from_str`, `chrono::FixedOffset` timezone (defaulting to UTC), and `MissedTickPolicy::{Skip, RunOnce}`.
2. `CronScheduler` accepting an `Arc<dyn LeaderElector>`, `role`, `candidate_id`, `lease_ttl`, and registered cron jobs (either enqueuing a `Job` to a `JobQueue` or executing an async handler closure).
3. Background leader-election + renewal loop (`try_acquire` / `renew` / `resign` on shutdown) ensuring only the active leader schedules/executes cron ticks, with automatic failover if leadership changes.
4. `CronSchedulerHandle::shutdown(timeout)` cleanly resigning leadership and awaiting background tasks.

## Constraints honoured

- **Leader election contract**: `crates/data/src/coordination/leader.rs` (`LeaderElector::try_acquire`, `renew`, `resign` — always resign on graceful shutdown).
- **Minimalism (`rules/85-minimalism.md`)**: Use `chrono::FixedOffset` already in workspace rather than adding `chrono-tz`.
- **TDD (`rules/50-agent-conduct.md`)**: Write hermetic tests against a mock/in-memory `LeaderElector` first, confirm fails-before, then implement.

## Named tests (bodies omitted)

- `cron_spec_rejects_invalid_expression_and_computes_next_tick_in_timezone` — validates `croner` parsing and `FixedOffset` next-occurrence calculation.
- `cron_scheduler_executes_only_on_elected_leader` — two `CronScheduler` instances sharing an in-memory `LeaderElector` execute each tick exactly once.
- `cron_scheduler_fails_over_when_leader_steps_down` — when the primary scheduler shuts down and resigns, the standby acquires leadership and resumes ticks.
- `cron_scheduler_respects_missed_tick_policy` — `Skip` jumps to future tick; `RunOnce` executes a single catch-up tick.

## Verification

```bash
cargo test -j 2 -p rume-messaging --lib cron --all-features
cargo clippy -j 2 -p rume-messaging --all-targets --all-features -- -D warnings
```

## Commit

Only when the user asks:

```text
feat(messaging): add leader-elected CronScheduler with timezone and missed-tick support
```

## Done-when

- [ ] `CronScheduler` parses 5/6-field cron expressions via `croner` with `FixedOffset` timezone support.
- [ ] Only the elected leader executes ticks; failover and `resign` on shutdown are tested.
- [ ] `MissedTickPolicy::{Skip, RunOnce}` behavior is verified by unit tests.
- [ ] Zero clippy warnings in `rume-messaging`.
