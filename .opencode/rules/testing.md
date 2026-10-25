# Testing convention (ENFORCED)

<!-- Rust-specific by default (Rust ships in this template); adapt placement
     rules to other stacks as needed. Full reference: `<tests README>` —
     create one as the suite grows and link it here. -->

## Rule

- **Pure domain / policy logic** → hermetic `#[cfg(test)]` unit tests inside the
  `src/` module it belongs to. No database, no app state, no connection pool.
  These run with `cargo test` and must pass offline.
- **Anything touching the database or the HTTP surface** → `tests/*.rs` integration
  test built on the shared harness (`tests/common`: one app/test-state builder,
  fixture seeders, db assert/reset helpers, a fluent HTTP client, fakes for
  out-of-process services). Never construct a raw pool or app state in a test body.
- **No silent skips.** If there's no DB, the harness helpers self-skip with a
  printed `[skipping …]` line. Never `return` silently. A printed skip is a
  report, not a pass: per `40-verification.md` the whole check still fails
  when a service is unreachable, unless skipping that suite was explicitly
  permitted for the run.
- **`#[ignore]` is temporary.** Reserve it for `todo_*` stubs and
  review-tracked known-broken tests (with a `"known-broken (<FINDING-ID>): …"`
  reason). The reason names the mechanism and the ruled-out alternatives per
  `80-quality-bar.md` — an ID alone is not enough. Never use it for aesthetics
  or to hide a red suite (see `85-minimalism.md`). Then **relocate**: extract the pure decision into a `pub(crate)` free
  fn with hermetic `src/` unit tests, make the minimal entry fn `pub` with a doc
  note only if a black-box test must call it directly, write the DB behavior as a
  `tests/` integration test, and drop `#[ignore]`.
- **Minimal `pub` exposure.** Widen visibility of a `src/` function only when an
  integration test must call it directly; add a `///` note that it exists for
  testing. Do not leak internals otherwise.
- **Fakes:** fake only out-of-process side effects (mail, push notifications,
  webhooks, external time). Never fake the database, the core engine, or real
  security-sensitive behavior (password hashing, token issuance).

## Check before writing a test

1. Does it need the database / app state / HTTP surface? → `tests/`, via the harness.
2. Is it a pure function of its inputs? → `src/` hermetic unit test.
3. Is it currently `#[ignore]`d because of a DB? → relocate per the recipe above.
   (Non-Rust stacks: same recipe, native idiom — skip markers are temporary.)

## Writing a good test

Two principles govern the test itself, independent of where it lives:

```
1. Every test names the break it catches
2. Every test exercises the real thing
```

**Name the break.** Before writing the body, answer: what production change
should make this test fail — and is that change a bug or a decision? Derive
expected values by hand (literals, hand-checked fixtures) — an expectation
computed by the code under test, or its helpers, passes no matter what that
code does. A test that fires only on redesign and sleeps through bugs (a
constant's exact value, exact message wording, private structure) is a
change detector, not a test — assert the behavior that depends on the
decision instead.

**Exercise the real thing.** Per `## Rule` above, fakes are for out-of-process
side effects only — but even inside that scope, a mock earns no assertions of
its own: assert the real component's behavior, and if the mock is what a test
is checking, unmock it or delete the assertion. Learn every side effect of the
real thing before replacing it, and mirror the complete real structure in any
mock response — a partial mock fails silently when downstream code reads a
field the mock omitted.

**The mutation check.** Before finishing, mentally mutate the code under
test — wrong constant, wrong branch, missing side effect, empty/default
return, missing validation for a boundary input. At least one test should
fail for each realistic mutation. A mutation nothing catches marks the
behavior as unprotected, or the test as tautological.

**Warning signs:**
- Setup and assertion share the same object, guaranteeing equality
- The test fails on every intentional change, never on accidental breakage
- Expected values are hidden behind loops, builders, or helpers
- The test greps source text instead of running it
- Mock setup is more than half the test, or you can't explain why it's needed
