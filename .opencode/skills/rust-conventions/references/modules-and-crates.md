# Modules and crates

## Module structure

- Modules should be small but usually bigger than a single item — one coherent concept (often one data type + its `impl`s and supporting types/functions), or a small themed collection of functions.
- `util` modules are a code smell — split them up and give the pieces better names.
- Declare most modules in their own file. Inline `mod foo { ... }` is fine for test-only modules (`#[cfg(test)] mod test { ... }`) or small modules used as unsafe boundaries.
- Put important items first — public before private, "top-down" structure.
- Put all test-only code in a single `#[cfg(test)] mod test`, rather than scattering `#[cfg(test)]` across items.
- Put `cfg`/`path` attributes on the module declaration; for other attributes, use inner attributes in out-of-line modules and outer attributes on inline modules.
- Modules should always have module-level documentation.
- Nested modules must be intuitively part of their enclosing module.
- Large-API crates/modules with a commonly-used subset should have a `prelude` module: public, top-level, re-exports only, no items with common/clashing names, no items needed only for special-case customization.

## Privacy and visibility

- Encapsulation works at the module level in Rust — don't try to replicate OO per-type encapsulation. Anything in a module (and its submodules) can read/write that module's private data.
- Avoid complex visibility modifiers (`pub(super)`, `pub(in path)`); `pub(crate)` is acceptable, but plain `pub` or private is better. Needing complex visibility is usually a sign the modules should be reorganized.
- Privacy is inherited: a public item inside a private module is still private outside it unless re-exported — so a path-scoped `pub(...)` outside the private module is redundant.
- Have a real reason for privacy (e.g. keeping `unsafe` code sound, maintaining an invariant) — not just a vague sense of encapsulation. Public fields are usually better than getters/setters.
- Make submodules public when the hierarchy is meant to be part of the API (like `std`); keep them private (using `pub use` to expose what's needed) when the hierarchy is an implementation detail — including at the crate root. Small crates: often re-export the whole API and keep all submodules private. Large crates: use public submodules and keep the crate root minimal (consider a prelude).
- Re-exporting from a dependency is fine if the item is logically part of your crate's API.
- An item should generally be publicly visible via a public module *or* a re-export — not both.

## What to import

- Import modules, types, and macros. Prefer *not* to import functions/constants — use qualified syntax (exception: very common functions like `std::thread::spawn`).
- If a module is used for one `impl` and would need many names imported, import just the module and qualify (e.g. `use std::fmt;` then `fmt::Formatter`, `fmt::Result`).
- Prefer qualification over renaming to disambiguate: `fmt::Result`, not `use std::fmt::Result as FmtResult`.
- Never import enum variants (qualify with the enum name) — except very common ones like `Some`/`Err`. When matching on a large enum inside a function, importing variants into that function's scope is OK if it's not confusing.
- Prefer importing macros over `extern crate`/`macro_import`.
- Import names at module level. Only import inside a function if the name is used only there and would clash at module level.
- Avoid glob imports — use them only to mean "bring in lots of items from this module," not as a way to avoid writing names out. Common exception: gloib-importing a prelude (`use futures::prelude::*;`), and `use super::*;` in most `test` modules.

## Crates

- Prefer small crates, without overdoing it.
- Every crate needs crate-level docs in its root module; library crates should also have a README.md.
- Use a Cargo workspace for multi-crate projects.
- Use Cargo build scripts (not makefiles) for build-time tasks, with proper directives (e.g. `println!("rerun-if-changed={}", file)`). Treat build scripts as real code — apply this whole guide to them.
- Use semver. Remember many "minor" Rust changes are actually breaking (e.g. adding a trait method).
- Export a minimal, coherent API (see the debuggability/dependability/flexibility/interoperability/predictability chapters of the Rust API guidelines).
- Avoid unstable features in published crates. They're OK in unpublished crates for large benefit — check the tracking issue first (is it near stabilization? known buggy? ask a Rust expert if unsure).
- Procedural macro crates: name them `foo-macros`; custom-derive crates: `foo-derive`. If a proc macro depends on another crate `bar`: don't emit `extern crate bar;` from the macro; re-export `bar` (and its public macros) from the main crate `foo`; use `::foo::bar...` inside macros; users should depend only on `foo`.

## Dependencies

Trade-offs to weigh when adding one:
- Using most of a crate is good for compile times vs. hand-rolling; using a small slice of a crate is worse.
- Well-used, maintained crates tend to be more robust than code written from scratch.
- Every external dependency is some risk (security, maintenance). An already-transitive dependency is nearly free. LTO makes the perf/size cost of a new dependency low.

Before adding a dependency, do a basic audit: license compatible? actively maintained, well used, well tested/CI'd? better alternatives? platform support? how much `unsafe` does it use, and does that look reasonable (no need to review every use)? what feature flags exist, and can you pick the smallest/most secure set?

Other rules:
- Run `cargo audit` for known vulnerabilities.
- Minimize duplicate versions of the same crate.
- Use the most up-to-date compatible version.
- Use `dev-dependencies`/`build-dependencies` where appropriate; make dependencies optional where possible.
- Path dependencies only for workspace crates. Prefer published versions over Git dependencies; if you must use a Git dependency, always pin a `rev`.
- Use the simplest version requirement possible (smallest correct version number, avoid needless point/patch pins, avoid semver operators where you can).

## Cargo.toml

- Alphabetize dependencies.
- Section order: `[package]` → `[lib]`/`[[bin]]` → `[features]` → `[workspace]` → `[dependencies]`/`[*.dependencies.*]` → `[dev-dependencies]`/`[*-dependencies]` → `[replace]`/`[patch]` → `[profile]`s. Separate sections with one blank line.
- Limit lines to 100 characters. Multi-line blocks: braces/brackets on their own lines, 4-space indent, no further nesting.
- Prefer `[patch]` over `[replace]`. Prefer `foo = { ... }` inline tables over `[dependencies.foo]` sections.
- Required package keys: `name`, `version`, `authors = ["The TiKV authors"]`, `license = "Apache-2.0"`, `edition = "2018"`.
- Crates published to crates.io also need: `description`, `keywords`, `homepage`, `repository`, `readme`.
- Crates not published need: `publish = false`.
- Feature names: kebab-case, never negative (`foo` not `no-foo`), short but descriptive.
- Avoid default features except for user-facing crates (binaries etc.) — default features complicate dependency graphs and can pull in unused features for downstream users, hurting compile times.
