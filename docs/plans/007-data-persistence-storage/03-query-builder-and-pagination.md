# Task 03 — Dynamic Query Builder (`sea-query`) & Pagination (`rume-data` + `rume-http`)

## Goal

Implement a composable, dialect-aware typed dynamic query builder wrapping `sea-query = "=1.0.2"` in `rume-data::persistence::builder` and standardized offset + cursor pagination in `rume-data::persistence::pagination` and `rume-http::pagination`.

## Files touched

- `crates/data/src/persistence/builder.rs`
- `crates/data/src/persistence/pagination.rs`
- `crates/data/src/persistence/mod.rs`
- `crates/http/src/pagination.rs`
- `crates/http/src/lib.rs`

## Deliverable

1. **`rume-data::persistence::builder`**:
   - `SqlDialect` (`Sqlite`, `Postgres`, `MySql`) with `SqlDialect::from_str_name(dialect: &str) -> Result<Self, PersistenceError>` matching `DatabaseBackend::dialect()`.
   - `BuiltQuery { sql: String, params: Vec<DbValue> }` with `.execute(&pool)`, `.execute_tx(&tx)`, `.fetch_all(&pool)`, `.fetch_all_as::<_, R>(&pool)`, `.fetch_optional_as::<_, R>(&pool)`, `.fetch_one_as::<_, R>(&pool)`, and `_tx` counterparts.
   - Typed wrappers around `sea_query::SelectStatement`, `InsertStatement`, `UpdateStatement`, and `DeleteStatement` (`DynamicSelect`, `DynamicInsert`, `DynamicUpdate`, `DynamicDelete`) that compile via `SqliteQueryBuilder`, `PostgresQueryBuilder`, or `MysqlQueryBuilder` and convert `sea_query::Values` losslessly into `Vec<DbValue>` (zero raw string concatenation).
2. **`rume-data::persistence::pagination`**:
   - `OffsetPageRequest { page: u64, per_page: u64 }` (clamped to safe bounds: `page >= 1`, `1 <= per_page <= max_per_page`) with `.offset()` and `.limit()`.
   - `OffsetPage<T> { items: Vec<T>, total: u64, page: u64, per_page: u64, total_pages: u64, has_next: bool, has_prev: bool }`.
   - `CursorPageRequest { cursor: Option<String>, limit: u64 }` and `CursorPage<T> { items: Vec<T>, next_cursor: Option<String>, prev_cursor: Option<String>, has_next: bool }`.
   - `CursorCodec` supporting both opaque URL-safe base64url JSON payloads and HMAC-SHA256 signed/verified cursors (`hmac = "=0.13.0"`, `sha2 = "=0.11.2"`, `subtle::ConstantTimeEq`) to prevent client cursor tampering.
   - `DynamicSelect::paginate_offset` and `DynamicSelect::paginate_cursor` helpers.
3. **`rume-http::pagination`**:
   - `OffsetPageQuery` and `CursorPageQuery` Axum `FromRequestParts` extractors (rejecting invalid bounds with `AppError::BadRequest` / `AppError::Validation`) implementing `HasOpenApiSchema`.
   - `PaginatedResponse<T>` and `CursorPaginatedResponse<T>` (`Serialize`, `HasOpenApiSchema`, `From<OffsetPage<T>>`, `From<CursorPage<T>>`, `IntoResponse` JSON).

## Constraints honoured

- `crates/data/src/persistence/pool.rs` & `backend/mod.rs`: execute only via parameterized `execute_params` / `fetch_all_as` / `fetch_optional_as` / `fetch_one_as` (`DbValue` bindings).
- `crates/security` / `rules/85-minimalism.md`: HMAC cursor verification MUST use `subtle::ConstantTimeEq`.
- `crates/http/src/openapi/spec.rs`: `HasOpenApiSchema` implementations must produce valid OpenAPI 3.1 schemas.

## Named tests (bodies omitted)

- `crates/data/src/persistence/builder.rs`:
  - `dynamic_select_compiles_for_sqlite_postgres_and_mysql_dialects` — verifies `$1`/`?` placeholders and quoting per dialect.
  - `dynamic_crud_executes_against_in_memory_sqlite` — runs `DynamicInsert`, `DynamicSelect`, `DynamicUpdate`, `DynamicDelete` against real SQLite.
- `crates/data/src/persistence/pagination.rs`:
  - `offset_page_computes_totals_and_clamps_bounds` — verifies `total_pages`, `has_next`, `has_prev`, and page/limit clamping.
  - `hmac_cursor_codec_roundtrips_and_rejects_tampering` — verifies signed cursor encode/decode and constant-time rejection of modified signature or payload.
- `crates/http/src/pagination.rs`:
  - `offset_page_query_extractor_parses_and_rejects_invalid_params` — verifies Axum `FromRequestParts` extraction and 400/422 rejection.
  - `paginated_response_generates_valid_openapi_schema` — verifies `HasOpenApiSchema` output for `PaginatedResponse<T>`.

## Verification

```bash
cargo test -j 2 -p rume-data --lib --all-features
cargo test -j 2 -p rume-http --lib --all-features
cargo clippy -j 2 -p rume-data -p rume-http --all-targets --all-features -- -D warnings
```

Expected: all builder, pagination, and OpenAPI schema tests pass with zero warnings.

## Commit

Only when the user asks:

```text
feat(data,http): add sea-query dynamic query builder and offset/cursor pagination
```

## Done-when

- [ ] `DynamicSelect`, `DynamicInsert`, `DynamicUpdate`, `DynamicDelete` compile and execute across dialects via `DbValue` bindings.
- [ ] `OffsetPage<T>` and HMAC-signed/opaque `CursorPage<T>` are implemented and tested.
- [ ] `rume-http::pagination` extractors and `HasOpenApiSchema` response wrappers pass unit tests.
