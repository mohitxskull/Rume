# Task 10 — Verify Phase 2

## Goal

Phase 2 acceptance: builds/tests green, depth ≤ 4, no leftover duplicated trees or stale
references.

## Files touched

None (verification only).

## Deliverable

Verification report (results of the commands below).

## Constraints honoured

- Global constraints: `cargo build` green; `cargo test -p rume-data -p rume-messaging -p rume-core`
  green; depth ≤ 4; single tree at `crates/data/sql/`.
- Conductor Phase 2 gate: `cargo build` + `cargo test -p rume-data -p rume-messaging - rume-core`
  green (gate from task 09); `find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1`
  ≤ 4; `find crates/messaging -name '*.sql'` empty; `grep -rn "persistence::sql" crates --include=*.rs`
  empty.

## Named tests — none new (the Phase 2 gate re-runs the Phase 2 suites: `rume-data`,
`rume-messaging`, `rume-core`)

None new; the Phase 2 gate re-runs the Phase 2 suites: `rume-data` (persistence backend +
migration + coordination + cache suites), `rume-messaging` (jobs/outbox/events suites),
`rume-core` (integration suites incl. `persistence.rs`, `sqlite.rs`, `enterprise.rs`).

## Verification

```
cargo build
cargo test -p rume-data -p rume-messaging -p rume-core
find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1    # expect: 4
find crates/messaging -name '*.sql'                                        # expect: (no output)
grep -rn "persistence::sql" crates --include=*.rs                          # expect: (no output)
grep -rn "rume_data::persistence::sql" crates --include=*.rs               # expect: (no output)
```

## Commit

Only when the user asks:

```
refactor(data,messaging): verify SQL tree centralization (build, tests, depth ≤ 4)
```

## Done-when

- [ ] `cargo build` exits 0.
- [ ] `cargo test -p rume-data -p rume-messaging -p rume-core` exits 0 (all suites).
- [ ] Deepest directory under `crates/data/sql/` has 4 components (`find crates/data/sql -type d | awk -F/ '{print NF}' | sort -n | tail -1` → `4`); deepest SQL path is `crates/data/sql/<dialect>/<file>.sql`.
- [ ] `find crates/messaging -name '*.sql'` returns nothing.
- [ ] `grep -rn "rume_data::persistence::sql" crates --include=*.rs` and `grep -rn "persistence::sql" crates --include=*.rs` return nothing.
- [ ] `find crates/data/sql -name '*.sql' | wc -l` = 109 (data tree 50 + messaging trees 59).
