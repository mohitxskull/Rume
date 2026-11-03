# Scope verification — v04 (§1.0)

Performed by the conductor before Stage 1 dispatch. Method: `find rume/src -type f`,
`find rume-cli -type f -not -path '*/target/*'`, `ls -a`, manifest checks.

## 1. `rume/src` top-level modules — all assigned

| Module / file | Assigned to |
|---|---|
| `app/` | Lens 7 (app-lifecycle) |
| `auth/` | Lens 5 (security-protocols) |
| `cache/` | Lens 4 (coordination-multi-tenant) |
| `config.rs` | Lens 6 (architecture-module-boundaries) |
| `coordination/` | Lens 4 |
| `enterprise/` | Lens 5 |
| `env/` | Lens 5 (secrets) + Lens 6 (loading/interpolation architecture) |
| `error.rs` | Lens 8 (routing-api-openapi) |
| `events/` | Lens 1 (outbox-data-integrity) |
| `helpers/` | Lens 5 (`safe_equal`, `html`, `mask`) + Lens 6 (remainder) |
| `jobs/` | Lens 3 (jobs-worker-concurrency) |
| `lib.rs` | Lens 6 |
| `metrics/` | Lens 8 |
| `module/` | Lens 6 |
| `openapi/` | Lens 8 |
| `outbox/` | Lens 1 |
| `persistence/` (incl. all `sql/`) | Lens 2 (geometry) + Lens 1 (`outbox_*.sql`) + Lens 3 (`jobs_*.sql`) + Lens 4 (`lock_*`/`leader_*.sql`) |
| `prelude.rs` | Lens 6 |
| `resilience/` | Lens 4 |
| `routing/` | Lens 8 |
| `saga/` | Lens 1 |
| `security/` | Lens 5 |
| `tenant/` | Lens 4 |
| `testing/` | Lens 10 (testing-feature-matrix) |
| `validation.rs` | Lens 8 |

No unassigned production subsystem found.

## 2. `rume-cli` — all assigned

- `src/` (main, cli, prompts, commands, ui, templates) → Lens 9 (dx-cli)
- `tests/generated_project.rs` → Lens 9
- `Cargo.toml` → Lens 9 (and Lens 6 for workspace wiring)

## 3. `rume/tests` — all assigned

- `integration.rs`, `containers.rs`, `integration/*.rs` (25), `containers/*.rs` (7), `common/` → Lens 10

## 4. Workspace manifests & CI

- Root `Cargo.toml`, `rume/Cargo.toml` → Lens 6 / Lens 10
- `rume-cli/Cargo.toml` → Lens 9
- No CI configuration present (`.github/` absent; no other CI dirs found). If added before the
  run, it is in scope per §1.0 step 3.

## 5. Out of scope (per plan §1.3, unchanged)

`lab/`, `target/`, `pack.md`, `pack.sh`, `opencode.sh`, `.opencode/`, `.zed/`, `.git/`,
`docs/`, `Cargo.lock`.

## Result

Every production subsystem in the declared code scope maps to a lens. Nothing silently
unreviewed. Stage 1 dispatch is cleared.
