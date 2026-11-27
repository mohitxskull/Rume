# Task 02 — `#[derive(FromRow)]` Proc-Macro (`rume-macros`)

## Goal

Implement `#[derive(FromRow)]` in `crates/macro/src/from_row.rs` supporting named structs, generic type parameters, `#[from_row(rename = "...")]`, `#[from_row(default)]`, `#[from_row(skip)]`, `#[from_row(flatten)]`, `#[from_row(json)]`, and `#[from_row(crate = "...")]`, with `trybuild` compile-fail diagnostics and runtime decoding tests.

## Files touched

- `crates/macro/src/lib.rs`
- `crates/macro/src/from_row.rs`
- `crates/macro/tests/from_row.rs` (new)
- `crates/macro/tests/compile_fail.rs`
- `crates/macro/tests/ui/from_row_*.rs` & `crates/macro/tests/ui/from_row_*.stderr` (new)

## Deliverable

1. `#[proc_macro_derive(FromRow, attributes(from_row))]` in `crates/macro/src/lib.rs` delegating to `from_row::expand`.
2. Container attribute `#[from_row(crate = "...")]` defaulting to `::rume::persistence` (allowing `#[from_row(crate = "rume_data::persistence")]` or custom paths).
3. Field attributes:
   - `rename = "col_name"`: decode from the specified column name instead of the Rust field name (stripping `r#` raw identifier prefix when unrenamed).
   - `default`: if the column is missing (`PersistenceError::ColumnNotFound`) OR SQL `NULL`, fall back to `Default::default()`; still propagate `PersistenceError::DecodeError` on corrupt non-null values.
   - `skip`: do not read any column; initialize with `Default::default()`.
   - `flatten`: delegate to `<FieldTy as FromRow>::from_row(row)?` for embedded structs.
   - `json`: decode column as `Option<String>` / `String` (or `Vec<u8>`) and deserialize via `serde_json::from_str` / `serde_json::from_slice`, mapping parse failures to `PersistenceError::DecodeError { column, message }` (supporting `Option<T>` when column is `NULL`).
4. Reject enums, unions, tuple structs, and conflicting field attributes (`skip` combined with `rename`/`flatten`/`json`, or `flatten` combined with `rename`/`json`) with clear `syn::Error` diagnostics.
5. Add generic type parameter bounds (`FromDbValue` for regular fields, `FromRow` for `flatten` fields, `serde::de::DeserializeOwned` for `json` fields, `Default` when `default`/`skip` is used on generic types).

## Constraints honoured

- `#![forbid(unsafe_code)]` in `crates/macro/src/lib.rs`.
- `crates/data/src/persistence/row.rs`: `DbRow::get`, `DbRow::get_opt`, `PersistenceError::ColumnNotFound`, and `PersistenceError::DecodeError` semantics must be preserved (especially `#[from_row(default)]` not swallowing `DecodeError`).
- `crates/macro/src/openapi_schema.rs`: follow the same `syn` attribute parsing and `crate = "..."` override conventions.
- `crates/macro/tests/compile_fail.rs`: follow existing `trybuild` UI test structure.

## Named tests (bodies omitted)

- `crates/macro/tests/from_row.rs`:
  - `from_row_decodes_named_struct_and_rename` — verifies basic fields, `r#type` raw ident stripping, and `#[from_row(rename = "...")]`.
  - `from_row_default_handles_missing_column_and_null_but_propagates_decode_error` — verifies `#[from_row(default)]` returns `Default::default()` on missing/null column and returns `DecodeError` on invalid type.
  - `from_row_skip_uses_default_without_touching_row` — verifies `#[from_row(skip)]` ignores row columns.
  - `from_row_flatten_decodes_nested_struct` — verifies `#[from_row(flatten)]` delegates to nested `FromRow`.
  - `from_row_json_deserializes_text_bytes_and_optional_null` — verifies `#[from_row(json)]` for `T` and `Option<T>`, and returns `DecodeError` on malformed JSON.
  - `from_row_generic_struct_with_bounds` — verifies generic `Wrapper<T>` derives `FromRow`.
- `crates/macro/tests/compile_fail.rs` (`trybuild`):
  - `from_row_enum_rejected` — compile-fail on enum.
  - `from_row_tuple_struct_rejected` — compile-fail on tuple struct.
  - `from_row_conflicting_attrs_rejected` — compile-fail on `#[from_row(skip, rename = "x")]`.

## Verification

```bash
cargo test -j 2 -p rume-macros --all-features
cargo clippy -j 2 -p rume-macros --all-targets --all-features -- -D warnings
```

Expected: all `from_row` unit tests and `trybuild` UI tests pass with zero warnings.

## Commit

Only when the user asks:

```text
feat(macro): implement #[derive(FromRow)] with rename, default, skip, flatten, and json
```

## Done-when

- [ ] `#[derive(FromRow)]` supports all specified attributes and generic structs.
- [ ] `trybuild` compile-fail tests pass for enums, tuple structs, and conflicting attributes.
- [ ] `cargo test -j 2 -p rume-macros --all-features` and clippy pass with zero warnings.
