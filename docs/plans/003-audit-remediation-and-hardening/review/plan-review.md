# Plan Review — Plan 003 (`003-audit-remediation-and-hardening`) — Final Re-review

## Summary

Plan 003 (`README.md` and Tasks `01`–`10`) resolves **M4** cleanly by placing `MysqlTransaction::execute_unvalidated` and `crates/data/src/persistence/mysql.rs` in Task 02 (`02`:13,24,31) and leaving Task 03 (`03`:12,21,26) to add `validate_managed_transaction_sql` enforcement on `MysqlTransaction::execute_params`. All prior findings (**C1**, **C2**, **M1–M4**, and **m1–m6**) are now verified resolved against `main` (`31683f6`) and installed dependencies, with zero remaining defects. **Verdict: `APPROVE`.**

## Prior decisions

- **M4 (`02`:13,24,31; `03`:12,21,26) — RESOLVED:** Prior finding **REAFFIRMED**. Moving `pub(crate) async fn execute_unvalidated` and `crates/data/src/persistence/mysql.rs` into Task 02 eliminates the forward compile dependency between Task 02 and Task 03. Because `crates/data/src/persistence/mod.rs:10` gates `pub mod mysql` behind `#[cfg(feature = "mysql")]`, `mysql_exec_tx_control` calls `execute_unvalidated` whenever `mysql.rs` is compiled, producing zero `dead_code` warnings under `-D warnings` while keeping Tasks `02` and `03` independently compilable and verifiable in sequence.
- **C1 (`02`:13,24,31; `03`:12,21,26) — RESOLVED:** `MysqlTransaction::execute_unvalidated` preserves `Migrator`'s MySQL `START TRANSACTION`/`COMMIT`/`ROLLBACK` control on the `GET_LOCK`-pinned connection without weakening public `validate_managed_transaction_sql` enforcement.
- **C2 (`10`:25-32,48,54,84) — RESOLVED:** Reactor-safe `PostgresSchemaFixture::drop` (`block_in_place` on `MultiThread`; fresh 1-connection `PostgresBackend::connect` in joined helper thread on `CurrentThread`) and `TestTransactionFixture<T>::drop` (`block_in_place` on `MultiThread`, joined helper thread for `sqlite`, non-blocking spawn + documented contract for `postgres`/`mysql` on `CurrentThread`).
- **M1 (`08`:20-25,38,44) — RESOLVED:** `prop.extract_with_context(&OtelContext::new(), ...)` + unconditional `span.set_parent(parent_cx)` before `span.context()` prevents ambient `OtelContext::current()` leakage on unheadered HTTP requests.
- **M2 (`README.md`:70; `09`:53,81) — RESOLVED:** `! rg -n 'include_str!\("\.\./\.\./\.\./data/' crates/` targets cross-crate `data/` paths without flagging intra-crate CLI generator templates (`crates/cli/src/commands/generate/{model,service,middleware}.rs:3`).
- **M3 (`05`:32-34,41,50-51,66,82) — RESOLVED:** Guarded `pcall(cjson.decode, data)` with `MALFORMED` checks and real Redis container test coverage across empty and populated `Session` rotation.
- **m1–m6 (`02`, `03`, `05`, `08`, `09`, `10`, `README.md`) — RESOLVED:** Index assertions (`persistence.rs:513-518`), 8 bind loops + `#[cfg]` gate + `#`/`[...]` lexing, `XmlNs` `<Issuer>` anchoring, `request_identity.rs:420-490` capture, `NewProjectDialect` `ValueEnum` + `new.rs:318`, and `crash_harness_binary()` `CARGO`/`current_dir`/`is_file()` are all accurately specified.

## Findings

None. All Critical, Major, and Minor findings from earlier rounds have been resolved without introducing regressions.

## Positive notes

- **End-to-end executability in strict task order:** Every task `01`–`10` now owns a self-contained `Files touched` set and verification gate with zero forward dependencies, zero stale Plan 002 artifacts, pinned dependencies (`proptest = "=1.12.0"`), and explicit `Why:` comments (`rules/70-code-rationale.md`) at every load-bearing decision site.

## Verdict

**APPROVE** — `docs/plans/003-audit-remediation-and-hardening/` is verified accurate against `main` (`31683f6`) and ready to present for user approval and execution.
