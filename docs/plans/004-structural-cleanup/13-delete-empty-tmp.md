# Task 13 — Delete the empty `./tmp/` directory

## Goal

Delete the empty `./tmp/` directory (confirmed at repo root, 0 entries).

## Files touched

- `./tmp/` — deleted (rmdir).

## Deliverable

`./tmp/` no longer exists at the repo root.

## Constraints honoured

- Global constraints: no behavior changes; `cargo build` stays green (no Rust code involved).
- Phase 3 decision: "delete the empty `tmp/`".

## Named tests — none (no code change; nothing exercises the empty `tmp/`)

None (no code change; no tests exercise `tmp/` — it is an empty directory).

## Verification

```
[ ! -d tmp ]
```

## Commit

Only when the user asks:

```
chore: delete empty ./tmp/ directory
```

## Done-when

- [ ] `[ ! -d tmp ]` is true (no `tmp` at the repo root).
- [ ] Nothing else in the tree references `tmp/` (no stale includes — it was empty).
