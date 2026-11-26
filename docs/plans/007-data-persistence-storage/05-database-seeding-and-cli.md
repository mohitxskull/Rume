# Task 05 — Database Seeding (`rume-data` + `rume-cli`)

## Goal

Implement idempotent, transaction-wrapped, dependency-ordered database seeding (`Seeder`, `SeedRegistry`, `SeedReport`) in `rume-data::persistence::seed` and the `rume db seed` subcommand in `crates/cli`.

## Files touched

- `crates/data/src/persistence/seed.rs`
- `crates/data/src/persistence/mod.rs`
- `crates/cli/src/cli.rs`
- `crates/cli/src/commands/db/mod.rs`
- `crates/cli/src/main.rs` (if hidden `db:seed` shortcut is dispatched there)

## Deliverable

1. **`rume-data::persistence::seed`**:
   - `Seeder` trait (`fn name(&self) -> &'static str`, `fn dependencies(&self) -> &[&'static str] { &[] }`, `fn run<'a>(&'a self, tx: &'a dyn DatabaseTransaction) -> BoxFuture<'a, Result<(), PersistenceError>>`).
   - `SeedRegistry` with `.register(seeder: impl Seeder + 'static)`:
     - Validates dependency graph via Kahn's topological sort; returns `PersistenceError::QueryFailed` / descriptive error on missing dependency or dependency cycle.
     - Creates `_rume_seeds` tracking table (`name TEXT PRIMARY KEY, applied_at TEXT NOT NULL`) dialect-safely (`Sqlite`, `Postgres`, `MySql`).
     - `.run_all(&pool) -> Result<SeedReport, PersistenceError>` and `.run_only(&pool, names: &[&str]) -> Result<SeedReport, PersistenceError>`: checks `_rume_seeds`, skips already-applied seeds (idempotent), executes each pending seeder inside its own `pool.transaction(...)` and records completion atomically in the same transaction.
   - `SeedReport { applied: Vec<String>, skipped: Vec<String> }`.
   - `SqlFileSeeder` helper wrapping a `(name, sql)` file for CLI/SQL-based seeds.
2. **`rume db seed` in `crates/cli`**:
   - Add `DbCommands::Seed { #[arg(long)] path: Option<String>, #[arg(long)] only: Option<Vec<String>> }` and hidden `db:seed` (`db-seed`) alias matching `db:status` / `db:migrate` / `db:rollback`.
   - Discover `.sql` files in `seeds/` (or `--path`), sort lexicographically, run through `SeedRegistry` against the configured `DatabaseConfig` (`Sqlite`, `Postgres`, `MySql`), and print a `TableBuilder` summary of applied vs skipped seeds.

## Constraints honoured

- `crates/cli/src/commands/db/mod.rs`: reuse existing `DatabaseConfig` loading and `TableBuilder` styling.
- `crates/data/src/persistence/pool.rs`: use `DatabasePool::transaction` for atomic per-seeder execution + ledger insert.
- `rules/50-agent-conduct.md`: no `#[ignore]` or silent skips in tests.

## Named tests (bodies omitted)

- `crates/data/src/persistence/seed.rs`:
  - `seed_registry_executes_in_dependency_order_and_is_idempotent` — runs dependent seeders twice against in-memory SQLite; asserts dependency order on first run and `skipped` on second run.
  - `seed_registry_detects_cycles_and_missing_dependencies` — verifies topological sort rejects cycles and unknown dependency names before touching the DB.
  - `seed_registry_rolls_back_failed_seeder_without_recording_ledger` — verifies failing seeder rolls back its transaction and is not recorded in `_rume_seeds`.
- `crates/cli/src/commands/db/mod.rs` (or `crates/cli/tests/`):
  - `cli_parses_db_seed_and_alias` — verifies `rume db seed --path ... --only ...` and `rume db:seed` CLI parsing.

## Verification

```bash
cargo test -j 2 -p rume-data --lib --all-features
cargo test -j 2 -p rume-cli --all-features
cargo clippy -j 2 -p rume-data -p rume-cli --all-targets --all-features -- -D warnings
```

Expected: all seeding and CLI parsing tests pass with zero warnings.

## Commit

Only when the user asks:

```text
feat(data,cli): add idempotent SeedRegistry and `rume db seed` command
```

## Done-when

- [ ] `Seeder`, `SeedRegistry`, and `SeedReport` support dependency ordering, cycle detection, idempotency, and per-seeder transactions.
- [ ] `rume db seed` discovers and executes SQL seeds with `TableBuilder` output.
- [ ] Tests and clippy pass with zero warnings.
