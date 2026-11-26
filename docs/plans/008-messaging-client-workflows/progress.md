# Plan ledger — plan: docs/plans/008-messaging-client-workflows/

- **Execution mode:** Strictly sequential on `main`, one implementer subagent at a time.
- **Review cadence:** Checkpoint review every 3 tasks (after Task 03 and Task 06) + whole-plan final review after Task 07.
- **Commits:** Authorized per task on `main` (local commits only, never push — under `/goal` run).
- **Base commit:** `571f9d6`.

## Execution Log
- Task 1: complete (571f9d6..0da5aad, checkpoint review batched after Task 3) — implementer `ffa2ac42-9b24-4738-a086-6f40a52b919f`
  - Ruling: Kept `crates/data/sql/postgres/jobs_schema.sql` immutable to preserve `20261104_001_rume_core_schemas` migration checksum; `queue` and `priority` columns are added via `PG_JOBS_QUEUE_PRIORITY` (`crates/data/sql/postgres/jobs_queue_priority.sql`) in `PostgresJobQueue::init_schema` — why: avoids breaking existing migration checksums on deployed DBs — cost if wrong: trivial (`init_schema` is idempotent).
- Task 2: complete (0da5aad..2d353cf, checkpoint review batched after Task 3) — implementer `a1eeb177-b4bc-4e8c-aab1-7fce5fe03f6d`
- Task 3: complete (2d353cf..8c0b504, checkpoint review 01–03 + fix round 1 `8c0b504..f0951d1` clean) — implementer `63ba24ea-ce6c-45ee-8ec8-ebc943e11b11`, reviewer `bcf31c34-0f72-4496-b7f6-d81174f5847b`, re-reviewer `eb94ab02-fd0a-4fcd-b3b7-e393dff0c602`
  - Task 2: minor (deferred): `JobWorker::run_loop` acquires per-queue semaphore after `dequeue_from` (head-of-line blocking if one capped queue is saturated).
  - Task 2: minor (deferred): `BatchBuilder::build_record` with 0 jobs initializes `BatchStatus::Running` with `pending_jobs: 0`.
  - Task 3: minor (deferred): `CronScheduler` awaits `(job.handler)().await` inline inside `run_job_loop`.
- Task 4: complete (f0951d1..e1c7e76, checkpoint review batched after Task 6) — implementer `88da9420-bdfb-436a-bb5b-fb4643c89e`
- Task 5: complete (e1c7e76..3f38d7f, checkpoint review batched after Task 6) — implementer `07cf69e9-3b53-41a4-858c-bb12a9daaf6f`
- Task 6: complete (3f38d7f..d98a06b, checkpoint review 04–06 clean) — implementer `37bdefc9-a2cd-4f11-9d8a-946539f92690`, reviewer `ae8902af-909f-4717-9419-719ca48735cb`
  - Task 5: minor (deferred): `DistributedEventBus::bridge` spawns a detached `tokio::spawn` loop without retaining a `JoinHandle` or shutdown signal.
  - Task 6: minor (deferred): `RabbitMqConsumer::next_message` acks immediately on receipt and `RabbitMqBroker::publish` opens a channel per publish.
- Task 7: complete (d98a06b..ece8055, whole-plan review APPROVE) — implementer `d3af4325-8826-44a1-8e64-05962e54abab`, final reviewer `9f0555a2-3300-405f-a654-468e0f722a40`
- **Plan 008 Status: COMPLETE** (`571f9d6..ece8055`).
