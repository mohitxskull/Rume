# Comments

- Prefer line comments (`//`, `///`) to block comments (`/* */`).
- Use doc comments (`///`) for module-level items. Plain `//` is acceptable for internal notes and FIXMEs, but consider putting that context in the docs too.
- Use `//!` only for file-level (crate/module) documentation; use `///` everywhere else.
- The first line of a doc comment should be a short, useful summary.
- Don't write comments that add no information (`Gets a Foo from self` on `get_foo`). Not everything needs a comment.
- A long comment on the module explaining core concepts is often more valuable than many short comments on individual functions — `std` does this well.
- Place comments above the code they annotate.
- Wrap comment lines at 100 characters.
- Document why a function might panic in a `# Panics` doc section.
- Document unsafe functions in a `# Safety` doc section: why it's unsafe and what invariants callers must uphold.
- Document every unsafe block with a `// Safety:` comment justifying correctness.
- Never commit commented-out code.

## Where/when to comment

Comment when there's context missing from the code, or hard to deduce from it:
- Important, obscure, tricky/interesting, or complex code.
- A known bug you can't fix or are deliberately ignoring right now.
- Suboptimal code you don't have a better solution for yet.
- Missing functionality or upcoming requirements not reflected in the code.

The last three should use `FIXME` (preferred over `TODO`, which is reserved for temporary personal reminders). File an issue and reference it: `FIXME(#1234): summary of the issue`.

Prefer explaining **why**, not what/how (which the code itself shows) — unless the code is genuinely complicated. Trade-offs and non-obvious performance considerations are especially worth writing down.

## Language for a good comment

- American English (color, canceling, synchronize — not colour, cancelling, synchronise).
- Correct spelling; standard/official capitalization (TiKV, TiDB-Binlog, Region, gRPC, RocksDB, GC, k8s, Prometheus Pushgateway — not Tikv, TiDB Binlog, region, grpc, k8S).
- Use words/expressions consistently — don't mix "dead link" and "broken link" in the same document.
- Avoid lengthy compound words; avoid unnecessary abbreviation.
- "we" refers only to the code writer *and* the reader.
- Standard grammar, correct punctuation, relatively short sentences.
- Capitalize the first letter, end with a period.
- Descriptive rather than imperative: "Opens the file" (good), "Open the file" (bad).
- Use "this" rather than "the" for the current thing: "Gets the toolkit for this component" (good).
- Markdown is allowed — use more of it for well-polished, published crates; less for internal/unpublished ones.
- Prefer rustdoc links (`` [`Foo`] ``, `` [`foo::bar::Baz`] ``) over raw URLs.

## Tips

- Comment while writing the code, not as an afterthought.
- Don't assume the code is self-evident.
- Skip unnecessary comments on simple code.
- Write comments as if they're for you, and keep them up to date as you edit.
- Include lots of examples; prefer that they're tested as doc tests.
