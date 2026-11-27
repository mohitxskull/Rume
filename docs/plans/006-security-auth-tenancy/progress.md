# Plan ledger — plan: docs/plans/006-security-auth-tenancy/

- **Execution mode:** Strictly sequential on `main`, one implementer subagent at a time.
- **Commits:** Authorized per task on `main` (local commits only, never push — under `/goal` run).
- **Base commit:** `9df5f1d`.

## Pre-flight scan

| Pair / Task | Shared File or Interface | Scan Result |
|---|---|---|
| `01` → `02` | `crates/security/Cargo.toml`, `crates/security/src/auth/{password,signed_token,mfa}.rs` | Clean: `01` pins `argon2 = "=0.6.0"` + crypto workspace deps and scaffolds compiling stubs. |
| `01` → `03` | `crates/security/src/auth/{jwt,oidc}.rs` | Clean: `01` pins `rsa`/`p256`/`p384`/`base64`/`reqwest` and scaffolds `JwtVerifier`/`OidcDiscovery` stubs. |
| `01` → `04` | `crates/data/sql/*`, `crates/security/src/auth/{api_key_store,sql_session_store,rbac}.rs` | Clean: `01` adds SQL assets across all 3 dialects with `tenant_id NOT NULL DEFAULT ''` & MySQL `VARCHAR(...)` (`M1`). |
| `01` → `05` | `crates/data/sql/postgres/pg_set_tenant_*.sql`, `crates/data/src/persistence/tenancy.rs`, `crates/security/src/tenant/*` | Clean: `01` adds PG RLS/`search_path` SQL assets (`m2`) and scaffolds `TenantPoolManager` & resolvers. |
| `01` → `06` | `Cargo.toml` (`saml-rs` `default-features = false`), `crates/enterprise/src/saml/*` | Clean: `01` disables `bergshamra/legacy-algorithms` (`C1`) and scaffolds `XmlDsigSamlVerifier` + `RedisSamlReplayCache`. |
| `02..06` → `07` | `crates/core/src/lib.rs`, `crates/core/Cargo.toml`, `crates/core/tests/integration/*` | Clean: `07` re-exports through `rume` facade and runs E2E + container tests (`m3`). |
| Self-consistency (`01`..`07`) | All 7 task files | Clean: Verified by `review/plan-review.md` and all `C1`, `M1`–`M3`, `m1`–`m3` findings resolved. |

## Execution Log
- Task 1: complete (9df5f1d..e12031c, review clean) — implementer `2ab06c46-2d1d-4297-9115-e78ce0fc1f90`, reviewer `10654528-5a53-42e2-b4af-4f865513c01c`
  - Task 1: note for Task 04: `auth_user_effective_permissions.sql` & `auth_user_roles_list.sql` currently filter `ur.tenant_id = $2/?` (single value); Task 04 should update them to include global roles (`IN ($2, '')` / `IN (?, '')`) or query both.
- Dep upgrade: complete (e12031c..6ea9683) — bumped all 11 outdated pins to latest stable (`tower-http 0.7.1`, `sqlx 0.9.1`, `redis 1.8.3`, `clap 4.6.10`, `toml 1.2.3+spec-1.1.0`, `sha2 0.11.2`, `rand 0.10.4`, `zeroize 1.9.2`, `uuid 1.29.3`, `clap_complete 4.6.12`, `rstest 0.29.0`); all workspace gates + `cargo deny` + `cargo audit` passed.
- Task 2: complete (6ea9683..a697130, checkpoint review batched after Task 3) — implementer `9330e6a9-c3c8-40f9-b747-d02a7cea8e2b`
- Task 3: complete (a697130..e8c2b3f, checkpoint review 02–03 clean) — implementer `6eaa3e19-54cb-43ba-88d9-dd575acd0add`, reviewer `647b3487-3acf-4837-b635-bb3b4c45d48b`
  - Task 2: minor (deferred): `TotpSecret::generate(config)` in `crates/security/src/auth/mfa.rs` does not validate `config` at construction time (validated on `generate_code`/`verify_code`).
- Task 4: complete (e8c2b3f..8228684, checkpoint review batched after Task 6) — implementer `92945a59-1768-461e-abb1-98faaf43eda7`
- Task 5: complete (8228684..5f0df61, checkpoint review batched after Task 6) — implementer `e0b36a8f-298f-4dd1-8ce5-d27b560ad629`
- Task 6: complete (5f0df61..a20a87f, checkpoint review 04–06 clean) — implementer `6b0b188e-02c2-4ac5-a365-8a179a33193f`, reviewer `e07b3510-6082-4df4-8c23-376038192df5`
  - Task 4: minor (deferred): `SqlApiKeyStore::rotate` has a harmless `let _ = issued.plaintext.expose_secret();` no-op and relies on `Drop` rollback on final `insert_stmt` error path.
- Task 7: complete (a20a87f..27c884c, whole-plan review APPROVE) — implementer `41ff4516-e2f4-43ca-9424-f6c146c26a86`, final reviewer `75b62e0e-cb64-4b29-8331-eea299878fe4`
- **Plan 006 Status: COMPLETE** (`9df5f1d..27c884c`).
