# Rule — Verification

**No "it works" claims without running the actual verification command and
confirming its output.** A verification that was not run is a verification that
failed.

**Hedging language is the tell.** "Should work", "probably passes", "looks
correct" — any of these before the command has actually run means stop and run
it. So does expressing satisfaction ("Great, that's fixed!") before the output
is in front of you: state the claim only after the evidence, not before.

## Run the whole check, not a grep for the thing you just changed

After any edit that moves or renames something, verify the whole file set. A
targeted grep confirms your fix and **cannot see what the fix broke elsewhere**.

| Subject | Whole check |
|---|---|
| Rust crate/workspace | fmt + clippy (zero warnings) + build + test — e.g. the project's clippy target (`make clippy` if defined, else `cargo clippy --workspace --all-targets`) `&& cargo fmt --all --check && cargo test`, or the project's cargo-make task if defined |
| Other builds | TODO — add one row per build/test surface, each a single runnable command |
| Documents | a sweep for duplicate definitions and dangling references across **every** file, not the one in hand |

## Services under test need the real thing running

Tests that touch a database, message broker, or network service verify nothing
against mocks alone. Start the real service, run the tests, fail loud when it
is unreachable — never let a skipped or mocked-out suite stand in for a run.

## Where checks live

New checks belong in the project's task runner / Makefile first, promoted to
scripts only when they need real control flow. Keep the whole check runnable
as **one command per subject**, so "run the whole check" stays cheap enough to
actually happen.
