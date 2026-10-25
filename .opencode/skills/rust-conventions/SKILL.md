---
name: rust-conventions
description: Apply the PingCAP/TiKV Rust style guide (https://pingcap.github.io/style-guide/rust/) when writing, editing, or reviewing Rust code. Use this whenever the user is writing new Rust code, refactoring or extending existing Rust code, or asks for a code review/critique of Rust — covering formatting, naming, comments, module/crate structure, data structure design, trait implementations, function/generic design, macros, expressions and control flow, error handling and panics, unsafe code, and performance. Also trigger on explicit mentions of "PingCAP style guide", "TiKV style", "idiomatic Rust", or "rust style guide". Do not trigger for non-Rust languages, or for pure Cargo/tooling questions unrelated to code style (e.g. "how do I publish to crates.io").
---

# PingCAP Rust Style Guide

This skill encodes the PingCAP/TiKV Rust style guide so Claude writes and reviews Rust
code consistently with it. The guide is opinionated on top of ordinary Rust idioms —
apply it whenever writing new Rust, editing existing Rust, or reviewing/critiquing Rust
code, not just when the user explicitly asks for a "style check."

Baseline tools: Rustfmt with **no custom config**, and Clippy with TiKV's disabled-lint
set (`make clippy` rather than `cargo clippy`, if the project has that target). Target
Rust 2018+ idioms (e.g. `crate::` imports, `use` for macros, no `extern crate` except for
widely-used macro crates).

## How to use this skill

The full guide is split into topic files under `references/`. Skim the checklist below
for the highest-signal rules — it's usually enough for routine writing and review. Read
a specific `references/*.md` file when the task concentrates in that area (e.g. designing
a new public API → `functions-and-generics.md` and `traits.md`; writing `unsafe` →
`unsafe-code.md`; a dedicated style-review request → read every reference file relevant
to the code in front of you before responding, not just the checklist).

| File | Covers |
|---|---|
| `references/formatting.md` | Rustfmt usage, import grouping/ordering |
| `references/naming.md` | Casing rules, method-name conventions (`new`, `as_`/`into_`/`to_`, `_mut`/`_ref`, etc.) |
| `references/comments.md` | Doc comments vs. line comments, `FIXME` vs `TODO`, comment language/tone |
| `references/modules-and-crates.md` | Module structure, privacy/visibility, imports, crate/dependency hygiene, Cargo.toml layout |
| `references/data-structures.md` | Structs, enums, tuples, unions, generics on data types, builder pattern |
| `references/traits.md` | `impl` ordering/grouping, trait semantics, common std traits (`Debug`, `Drop`, operator traits, etc.) |
| `references/functions-and-generics.md` | Function/method design, conversions, generics, `impl Trait` vs `dyn Trait`, trait design |
| `references/macros.md` | When (not) to write a macro, declarative vs. procedural |
| `references/expressions-and-statements.md` | Shadowing, mutability, iteration, `match`/`if` style, panicking basics |
| `references/errors-and-panics.md` | `Result`/error type design, `?` vs `unwrap`/`expect` |
| `references/unsafe-code.md` | `unsafe` documentation and invariant boundaries |
| `references/performance.md` | When (not) to optimize, avoiding hidden cost |

## Quick-reference checklist

Use this as a first pass when writing or reviewing code. It covers the rules that come
up most often; read the linked reference file for the full rationale or edge cases.

**Formatting & imports**
- Rustfmt, unmodified config. Unix line endings.
- Imports: absolute paths (`crate::...`, not `super::...`/`self::...`, except relative
  re-exports like `pub use self::foo::bar;`); one nested `use` per crate; imports grouped
  together, re-exports grouped separately.

**Naming**
- `UpperCamelCase` types/traits/variants, `snake_case` everything else, `SCREAMING_SNAKE_CASE`
  consts. Full words over abbreviations unless the abbreviation is ecosystem-standard
  (`ctx`, `expr`). Acronyms treated as words (`GrpcType`, `Tikv` — not `GRPCType`, `TiKV`).
- Conversions: `as_x` (cheap, borrowed), `into_x` (consuming, no clone), `to_x` (expensive/clones).
- Constructors: `new` (primary), `with_x` (secondary), builder pattern for real complexity.
- No getters/setters — prefer public fields; if you must, `foo`/`set_foo` (never `get_foo`).

**Comments**
- `///` doc comments on public items; `//!` only for file-level docs. Explain *why*, not
  *what*. `FIXME(#issue)` over bare `TODO`. Never commit commented-out code.

**Structure**
- Small, single-concept modules; avoid `util` grab-bags. Prefer public fields and simple
  `pub`/`pub(crate)` visibility over `pub(super)`/complex path visibility.
- Small structs composed together; enums with a natural null get their own `None` variant
  rather than being wrapped in `Option`.

**Functions & traits**
- Short functions; constructors are static methods; no-arg constructor → `Default`.
- Prefer borrowing unless the function needs ownership; return by value, not `Box`/`Rc`.
- Prefer `From`/`TryFrom` over ad hoc conversion methods; prefer a method when there's a
  clear `self` receiver.

**Errors & panics**
- `?`, not `try!` or ignored `Result`s. Error types: `Debug + Display + std::error::Error
  + Send + Sync`. Prefer `Result` over panicking; prefer `expect("msg")` over bare `unwrap()`
  outside tests.

**Unsafe**
- Avoid it; when used, document *every* block/fn with what makes it safe and what
  invariants the caller must uphold. See `references/unsafe-code.md` before writing or
  reviewing any `unsafe`.

**Performance**
- Don't optimize without profiling data, but don't write carelessly wasteful code either
  (avoid needless clones/allocations, use iterators, don't hide blocking/IO/alloc costs).

## When reviewing code

Read the code with the checklist and the relevant reference files in mind, then report
findings grouped by file/section rather than as a flat list, and note *why* each item
matters (most reference files include a short rationale — reuse it rather than just
asserting a rule). Distinguish clear guide violations from stylistic judgment calls where
the guide itself says "consider" or "prefer" rather than stating an absolute rule — call
out the latter more gently, since the guide leaves room for engineering judgment there.

## When writing code

Apply the checklist as you go rather than writing first and fixing style after. For
anything non-routine — a new public API, a data structure with several variants, generic
function signatures, or any `unsafe` — read the matching reference file before writing,
since the details (e.g. exact `impl` ordering, or what unsafe invariants need documenting)
are easy to get subtly wrong from memory.
