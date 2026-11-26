# Task 02 — `rume openapi` & TypeScript SDK Codegen

## Goal

Export the application's OpenAPI v3.1 JSON specification via `ApplicationMode::OpenApi` and `rume openapi export`, and generate a typed TypeScript interfaces + `fetch` client SDK via `rume openapi ts`.

## Files touched

- `crates/foundation/src/state.rs`
- `crates/app/src/app/builder/mod.rs`, `crates/app/src/app/builder/router.rs`
- `crates/cli/src/cli.rs`, `crates/cli/src/commands/mod.rs`, `crates/cli/src/commands/openapi/mod.rs`, `crates/cli/src/commands/openapi/ts.rs`

## Deliverable

- `ApplicationMode::OpenApi` added to `crates/foundation/src/state.rs` (preserving `Copy`).
- `RumeBuilder` stores optional `openapi_info: Option<OpenApiInfo>` (populated by `.openapi(path, info)` or `.openapi_info(info)`, defaulting to `OpenApiInfo::new("Rume API", "0.1.0")` during OpenAPI export).
- `RumeBuilder::effective_mode()` recognizes `RUME_INSPECT=openapi`, `--rume-inspect-openapi`, and `.mode(ApplicationMode::OpenApi)` as `ApplicationMode::OpenApi`.
- `run_openapi_mode()` resolves modules, collects/validates routes (without starting the server or running `on_start`/`on_ready` side effects), generates `OpenApiSpec` via `route_registry.to_openapi(info)`, and prints `__RUME_OPENAPI_JSON_START__\n...\n__RUME_OPENAPI_JSON_END__`.
- `crates/cli/src/cli.rs` and `crates/cli/src/commands/openapi/mod.rs` implementing:
  - `rume openapi export [--bin <name>] [--out <path>]` — extracts OpenAPI v3.1 JSON from the project binary and writes to stdout or `--out`.
  - `rume openapi ts [--bin <name>] [--input <path>] [--out <path>]` — reads OpenAPI JSON from `--input` (offline) or from the project binary, and writes generated TypeScript to stdout or `--out`.
- `crates/cli/src/commands/openapi/ts.rs`: pure-Rust OpenAPI v3.1 -> TypeScript generator:
  - Maps OpenAPI primitive/array/object/format schemas (`string`, `integer`, `number`, `boolean`, `array`, `object`, `required` vs optional `?`, nested objects, `$ref` / `components.schemas`) to TypeScript types and `export interface` blocks.
  - Generates a typed `RumeApiClient` (or `createClient(baseUrl, fetchImpl?)`) with one async method per operation (`operationId` or deterministic `<method>_<path>` camelCase name), typed path params, optional query params object, typed JSON request body, and typed `Promise<T>` response handling non-2xx errors.

## Constraints honoured

- `crates/http/src/openapi/spec.rs` (`OpenApiSpec`, `OpenApiInfo`, `OpenApiSchema`).
- `crates/app/src/app/builder/router.rs` (`RumeBuilder::openapi`).
- `crates/cli/src/commands/list_routes.rs` (tagged stdout extraction).
- `rules/50-agent-conduct.md` (TDD), `rules/80-quality-bar.md`, `rules/85-minimalism.md`.

## Named tests (bodies omitted)

- `app::builder::tests::openapi_mode_collects_app_and_module_routes_without_binding` — builds a `RumeBuilder` with app + module routes in `ApplicationMode::OpenApi`, runs it, and verifies `Ok(())` without starting hooks or binding a port.
- `commands::openapi::ts::tests::generates_interfaces_for_object_schemas_and_required_fields` — passes an OpenAPI JSON with `components.schemas` and inline object schemas (required + optional fields, arrays, numbers, booleans) and asserts generated `export interface` blocks and `?` optionality.
- `commands::openapi::ts::tests::generates_client_methods_for_path_query_body_and_responses` — passes an OpenAPI JSON with `GET /users/{id}` (path + query params, `200` JSON schema) and `POST /users` (JSON `requestBody`, `201` response) and asserts typed method signatures, URL interpolation, query serialization, and `fetch` invocation.
- `commands::openapi::ts::tests::sanitizes_operation_ids_and_handles_empty_paths` — verifies deterministic valid TypeScript identifiers when `operationId` is absent or contains hyphens/slashes, and clean output for empty `paths`.

## Verification

```bash
cargo fmt --all --check && \
cargo clippy -j 2 -p rume-foundation -p rume-app -p rume-cli --all-targets --all-features -- -D warnings && \
cargo test -j 2 -p rume-foundation -p rume-app -p rume-cli --lib --all-features
```
Expected: all commands exit 0 with zero warnings and all tests passing.

## Commit

Only when the user asks:
```text
feat(app,cli): add ApplicationMode::OpenApi, `rume openapi export`, and TypeScript SDK codegen
```

## Done-when

- [ ] `ApplicationMode::OpenApi` emits valid OpenAPI v3.1 JSON for app and module routes.
- [ ] `rume openapi export` and `rume openapi ts` are wired in `rume-cli` and support both live binary inspection and `--input <file>`.
- [ ] Hermetic unit tests cover schemas, path/query params, request bodies, and responses.
- [ ] Verification command passes cleanly.
