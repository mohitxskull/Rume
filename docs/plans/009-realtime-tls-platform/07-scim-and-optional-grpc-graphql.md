# Task 07 — Enterprise SCIM 2.0 HTTP Protocol & Optional gRPC / GraphQL

## Goal

Implement the RFC 7644 SCIM 2.0 filter parser, `ScimStore` (`InMemoryScimStore`), and Axum `/scim/v2/Users` and `/scim/v2/Groups` CRUD handlers (`rume-enterprise` + `rume-http`), plus optional `rume-grpc` (`tonic`) and `rume-graphql` (`async-graphql` + `async-graphql-axum`) crates sharing Rume's W3C tracing, `AppError` mapping, and Axum router integration.

## Files touched

- `crates/enterprise/src/enterprise/scim/mod.rs`, `filter.rs`, `store.rs` (refactoring `scim.rs` into a module while preserving all existing public exports)
- `crates/http/src/scim/mod.rs`
- `crates/grpc/src/lib.rs` (and submodules `error.rs`, `trace.rs`, `router.rs`)
- `crates/graphql/src/lib.rs` (and submodules `error.rs`, `trace.rs`, `router.rs`)

## Deliverable

- **SCIM 2.0 (`rume-enterprise::enterprise::scim` & `rume-http::scim`)**:
  - `ScimFilter` AST + recursive-descent RFC 7644 §3.4.2.2 parser (`eq`, `ne`, `co`, `sw`, `ew`, `pr`, `gt`, `ge`, `lt`, `le`, `and`, `or`, `not`, parentheses) with bounded `max_depth` and `max_filter_len` (returning `400 Bad Request` `ScimErrorResponse` on excess to prevent stack-overflow DoS) + `matches_user(&ScimUser)` / `matches_group(&ScimGroup)`.
  - `ScimStore` trait (`create_user`, `get_user`, `replace_user`, `patch_user`, `delete_user`, `list_users`, and `Group` equivalents) + `InMemoryScimStore` supporting `filter`, `startIndex`, `count`, and `meta.version` ETag updates.
  - `scim_routes(store)` in `rume-http::scim` (gated behind the `scim` feature on `rume-http` and `rume`) exposing `/scim/v2/Users`, `/scim/v2/Users/{id}`, `/scim/v2/Groups`, `/scim/v2/Groups/{id}` with `application/scim+json` content-type and RFC 7644 `ScimErrorResponse` status mapping (`400`, `404`, `409`).
- **Optional `rume-grpc` (`crates/grpc`)**:
  - `app_error_to_status(err: &AppError) -> tonic::Status` mapping all `AppError` variants to canonical gRPC status codes.
  - `RumeTraceInterceptor` extracting/injecting W3C `traceparent`/`tracestate` and `x-request-id` from `tonic::metadata::MetadataMap` into `rume_observability::TraceContext`.
  - `GrpcRouterExt` / `mount_grpc_routes` integrating `tonic::service::Routes` with Axum `Router`.
- **Optional `rume-graphql` (`crates/graphql`)**:
  - `GraphQlConfig` (`path`, `graphiql_enabled`, `max_depth`, `max_complexity`).
  - `app_error_to_graphql(err: &AppError) -> async_graphql::Error` attaching structured `code` / `status` extensions.
  - `RumeTraceExtension` (`async_graphql::extensions::Extension`) wrapping GraphQL execution in a tracing span linked to `TraceContext::current()`.
  - `graphql_routes(schema, config)` returning an Axum `Router` serving POST queries/mutations and optional GraphiQL GET UI.

## Constraints honoured

- `crates/enterprise/src/enterprise/scim.rs` (preserve all existing `ScimUser`, `ScimGroup`, `ScimListResponse`, `ScimErrorResponse` types and tests).
- `crates/foundation/src/error.rs` (`AppError` variants).
- `crates/observability/src/trace_context.rs` (`TraceContext`).
- `#![forbid(unsafe_code)]`, `rules/80-quality-bar.md`.

## Named tests (bodies omitted)

- `rume_enterprise::enterprise::scim::tests::scim_filter_parses_compound_boolean_and_evaluates_users` — parses `userName eq "alice" and (active eq true or emails.value co "@example.com") and not (displayName sw "Test")` and asserts matching/non-matching `ScimUser`s.
- `rume_http::scim::tests::scim_http_users_and_groups_crud_and_filter_roundtrip` — creates, lists with `?filter=...&startIndex=1&count=10`, patches, replaces, and deletes Users and Groups over Axum `oneshot`, asserting `application/scim+json` and `404` after delete.
- `rume_grpc::tests::grpc_maps_app_error_to_status_and_propagates_w3c_trace_context` — verifies `AppError` -> `tonic::Status` mapping and `RumeTraceInterceptor` extracting `traceparent`.
- `rume_graphql::tests::graphql_router_executes_query_and_maps_app_error_extensions` — executes a query and an erroring resolver through `graphql_routes` via Axum `oneshot`, asserting JSON data and `AppError` extension code.

## Verification

```bash
cargo test -j 2 -p rume-enterprise -p rume-http -p rume-grpc -p rume-graphql --all-features
```
Expected: all SCIM, gRPC, and GraphQL tests pass with zero warnings.

## Commit

Only when the user asks:
```text
feat(enterprise,http,grpc,graphql): add SCIM 2.0 HTTP protocol and optional gRPC/GraphQL crates
```

## Done-when

- [ ] RFC 7644 SCIM filter parser, `ScimStore`, and `/scim/v2/Users` & `/scim/v2/Groups` Axum handlers work end-to-end.
- [ ] `rume-grpc` maps `AppError` to `tonic::Status`, propagates W3C `TraceContext`, and mounts into Axum.
- [ ] `rume-graphql` executes `async-graphql` schemas on Axum with `AppError` extensions and `TraceContext` spans.
