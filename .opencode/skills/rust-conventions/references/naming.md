# Naming

**Prefer meaningful names to short names.**

## Casing

| Item | Case |
|---|---|
| Types and traits | `UpperCamelCase` |
| Enum variants | `UpperCamelCase` (never prefixed with the enum name — `Some`, not `OptionSome`) |
| Struct fields | `snake_case` |
| Functions and methods | `snake_case` |
| Local variables | `snake_case` |
| Macros | `snake_case` |
| Constants (`const`, immutable `static`) | `SCREAMING_SNAKE_CASE` |
| Crate names | `kebab-case` in Cargo.toml, `snake_case` in Rust code (prefer single-word names) |
| Modules | `snake_case` |

- All names should be English.
- Reserved words (`type`, `crate`): use an abbreviation (`ty`) or trailing underscore (`crate_`). Use raw identifiers (`r#type`) only when necessary (generated code, or code that interacts with data via identifier names, e.g. JSON/protobuf).
- Prefer full words to abbreviations (`diagnostic`, not `diag`). Abbreviations that are well-known and standard outside the codebase are fine (`ctx`, `cf`, `expr`). When in doubt, spell it out.
- Use standard acronyms as words: `Sql` not `StandardQueryLanguage`. Treat acronyms as ordinary words for casing — `GrpcType`/`grpc_variable`, not `GRPCType`/`GRPC_variable`. `TiKV` is capitalized as `Tikv` (e.g. `TikvServer`, not `TiKVServer`). Treat multi-word contractions as one word: `Stdin`, not `StdIn`.
- Short (even single-character) names are fine for local variables with very narrow, obvious scope:
  ```rust
  vec.iter().map(|s| s.len());          // closure arg
  match ... { Err(e) => println!("Error: {}", e), .. }  // single-line match arm
  for (i, label) in labels.iter().enumerate() { .. }    // loop counter
  ```
- When interfacing with non-Rust code, use Rust naming conventions in the Rust code even if that means Rust and non-Rust names diverge (correlation should stay clear). Use aliases/wrappers to avoid unconventional FFI names.
- Generic type/lifetime parameters: short (usually single-letter) names like `T`, `'a`. Use longer names only when multiple generics in scope make short names confusing. Associated types get descriptive names like any other type.

## Method names

- Primary constructor: `new`. May take all fields or supply defaults.
- Secondary constructors that customize more than `new`: prefix `with_` (e.g. `with_capacity`).
- Cheap, non-consuming conversion to a reference of another type: `as_type` (e.g. `as_str`).
- Consuming conversion to an owned value that never clones (except cheap `Copy` types): `into_type` (e.g. `into_string`).
- Expensive conversion (e.g. clones): `to_type` (e.g. `to_string`).
- Consuming a wrapper to return the inner value: `into_inner` if it never panics, `unwrap` if it might. Non-consuming reference access: `get`. Otherwise follow `Box`/`Option`/`Result` conventions.
- Default iteration method: `iter`. A consuming iteration method should usually come from implementing `IntoIterator` (named `into_iter`).
- Avoid getters/setters; prefer public fields. If you must: `foo` / `set_foo` (not `get_foo`). Presence checks: `is_`/`has_` prefix, return `bool`.
- Trait names: prefer (transitive) verbs, nouns, then adjectives; avoid `-able` suffixes. E.g. `Copy`, `Send`, `Encode`.
- Ownership/mutability variants use suffixes: `foo` (default), `foo_ref` (by-reference), `foo_mut` (by-mutable-reference). Never stack suffixes (`foo_mut_ref` is wrong).
- Mutability usually doesn't need to appear in a name. Where needed to disambiguate return types, `mut` is a *prefix*: `as_mut_slice`, not `as_slice_mut`.
