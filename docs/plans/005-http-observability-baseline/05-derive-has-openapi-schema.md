# Task 05 — `#[derive(HasOpenApiSchema)]` Procedural Macro

## Goal

Implement `#[derive(HasOpenApiSchema)]` in `rume-macros` (`crates/macro/src/openapi_schema.rs`) so any struct can automatically derive `HasOpenApiSchema` — reflecting Rust field types, `Option<T>` optionality, `#[serde(...)]` field renames/skips, and common `#[garde(...)]` validation rules directly into `OpenApiSchema` — with unit and `trybuild` compile-fail tests.

## Files touched

- `crates/macro/src/openapi_schema.rs`
- `crates/macro/tests/openapi_schema.rs` (new)
- `crates/macro/tests/ui/openapi_union_rejected.rs` (new)
- `crates/macro/tests/ui/openapi_union_rejected.stderr` (new)
- `crates/macro/tests/ui/openapi_tuple_struct_rejected.rs` (new)
- `crates/macro/tests/ui/openapi_tuple_struct_rejected.stderr` (new)

## Deliverable

1. **Struct derivation (`crates/macro/src/openapi_schema.rs`)**:
   - Supports structs with named fields (generating `OpenApiSchema::object()` with `.with_required_property(...)` for non-`Option<T>` fields and `.with_property(...)` for `Option<T>` fields, delegating each field's base schema to `<FieldType as HasOpenApiSchema>::schema()`).
   - Supports unit structs (empty `OpenApiSchema::object()`).
   - Rejects unions, enums (unless simple string-only unit enums — if unsupported, return a clear `syn::Error` with span), and tuple structs with helpful `syn::Error` messages rather than panicking.
2. **Attribute support**:
   - `#[openapi(crate = "rume_http::openapi")]` (container attribute): overrides the default trait/schema path `::rume::openapi` so both `rume` facade consumers and `rume-http`-only consumers can use the derive cleanly.
   - `#[serde(...)]`:
     - Container `#[serde(rename_all = "...")]` supporting `camelCase`, `snake_case`, `PascalCase`, `kebab-case`, `SCREAMING_SNAKE_CASE`, `lowercase`, `UPPERCASE`.
     - Field `#[serde(rename = "...")]` (overrides `rename_all`).
     - Field `#[serde(skip)]` and `#[serde(skip_deserializing)]` (omits field from schema).
     - Field `#[serde(default)]` (treats field as optional, i.e. `.with_property` instead of `.with_required_property`).
   - `#[garde(...)]` (field attributes — mirrors `crates/http/src/validation.rs:549-557` manual impl):
     - `email` -> `.with_format("email")`
     - `url` -> `.with_format("uri")`
     - `length(min = N, max = M)` (either or both, including `chars(min = ..., max = ...)`) -> `.with_min_length(N)` / `.with_max_length(M)`
     - `range(min = N, max = M)` (either or both, integer literals only — non-integer bounds such as `f64` or non-literal expressions are ignored gracefully rather than emitting uncompilable `with_minimum`/`with_maximum` calls, since `OpenApiSchema::with_minimum`/`with_maximum` take `i64` at `crates/http/src/openapi/spec.rs:186-194`) -> `.with_minimum(N)` / `.with_maximum(M)`
     - `pattern("...")` -> `.with_pattern("...")`
     - Unknown or custom `#[garde(...)]` rules (e.g. `custom(...)`, `dive`, `skip`) are ignored gracefully without failing compilation.
   - Generic structs: preserves generics and adds `T: HasOpenApiSchema` bounds on type parameters used in non-skipped fields.
3. **Tests**:
   - Pure unit tests in `crates/macro/src/openapi_schema.rs` testing `Option<T>` detection, `rename_all` casing transformations, and `#[garde(...)]` attribute parsing.
   - `trybuild` compile-fail tests in `crates/macro/tests/openapi_schema.rs` verifying clear diagnostics on unions and tuple structs. (Full runtime schema assertions with `rume::openapi::HasOpenApiSchema` are exercised in Task `07` inside `crates/core/tests/integration/http_baseline.rs`, avoiding a cyclic dev-dependency from `rume-macros` to `rume`.)

## Constraints honoured

- `#![forbid(unsafe_code)]` (`crates/macro/src/lib.rs:3`).
- `syn = "=3.0.10"`, `quote = "=1.0.47"`, `proc-macro2 = "=1.0.107"` (no new dependencies in `crates/macro/Cargo.toml`).
- No dependency from `rume-macros` on `rume-http` or `rume` (preserves strict DAG).
- `rules/70-code-rationale.md`: document `Why:` on `#[openapi(crate = "...")]` default (`::rume::openapi`, matching `sql_assets!` and `embed_migrations!` in `crates/macro/src/lib.rs:378,619`).

## Named tests (bodies omitted)

In `crates/macro/src/openapi_schema.rs`:
- `rename_all_transforms_snake_case_to_all_supported_conventions` — tests `camelCase`, `PascalCase`, `kebab-case`, `SCREAMING_SNAKE_CASE`, `lowercase`, `UPPERCASE`.
- `is_option_type_recognizes_option_variants` — recognizes `Option<String>`, `std::option::Option<i32>`, `core::option::Option<T>`, and rejects `Vec<Option<String>>` and `MyOption<T>`.
- `parse_garde_rules_extracts_email_url_length_range_and_pattern` — parses `#[garde(email)]`, `#[garde(length(min = 3, max = 50))]`, `#[garde(range(min = 18, max = 120))]`, `#[garde(pattern("^[a-z]+$"))]`, and ignores `#[garde(dive)]` / `#[garde(custom(my_fn))]`.

In `crates/macro/tests/openapi_schema.rs`:
- `derive_has_openapi_schema_compile_fail_diagnostics` — runs `trybuild::TestCases` against `tests/ui/openapi_union_rejected.rs` and `tests/ui/openapi_tuple_struct_rejected.rs`.

## Verification

```bash
cargo fmt --all --check
cargo clippy -j 2 -p rume-macros --all-targets -- -D warnings
cargo test -j 2 -p rume-macros --all-targets
```

Expected: zero warnings; all unit tests and `trybuild` compile-fail tests pass.

## Commit

*Only when the user asks:*

```text
feat(macro): implement #[derive(HasOpenApiSchema)] with serde and garde support
```

## Done-when

- [ ] `crates/macro/src/openapi_schema.rs` implements `#[derive(HasOpenApiSchema)]` with zero `todo!()` stubs remaining.
- [ ] Supports named structs, `Option<T>`, `#[openapi(crate = "...")]`, `#[serde(rename/rename_all/skip/default)]`, and `#[garde(email/url/length/range/pattern)]`.
- [ ] All unit tests and `trybuild` compile-fail tests pass under `cargo test -j 2 -p rume-macros --all-targets`.
