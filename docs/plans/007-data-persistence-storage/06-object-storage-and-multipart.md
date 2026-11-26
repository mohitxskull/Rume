# Task 06 — Object Storage (`LocalFsObjectStore` + `S3ObjectStore`) & Multipart Uploads (`rume-data` + `rume-http`)

## Goal

Implement the unified `ObjectStore` trait with `LocalFsObjectStore` and pure-Rust SigV4 `S3ObjectStore` in `rume-data::storage`, and streaming multipart file upload extraction/validation in `rume-http::upload`.

## Files touched

- `crates/data/src/storage/mod.rs`
- `crates/data/src/storage/local.rs`
- `crates/data/src/storage/s3.rs` (`#[cfg(feature = "s3")]`)
- `crates/data/src/lib.rs`
- `crates/http/src/upload.rs`
- `crates/http/src/lib.rs`

## Deliverable

1. **`rume-data::storage`**:
   - `ObjectKey` validated key wrapper (rejects empty keys, leading `/`, `\`, null bytes, and `..` path segments).
   - `ObjectMeta { key: String, size: u64, content_type: Option<String>, etag: Option<String>, last_modified_epoch_secs: Option<i64> }`.
   - `PutObjectOptions { content_type: Option<String> }` and `PresignedUrl { url: String, method: &'static str, expires_in_secs: u64 }`.
   - `ObjectStoreError` (`NotFound`, `InvalidKey`, `PermissionDenied`, `Io`, `Http`, `Unsupported`, `Configuration`).
   - `ObjectStore` trait (`put`, `get`, `head`, `delete`, `list`, `presign_get`, `presign_put`) returning `BoxFuture<'_, Result<..., ObjectStoreError>>`.
   - `LocalFsObjectStore`: async `tokio::fs` implementation rooted at `base_dir`, enforcing `ObjectKey` validation + canonicalized root containment (on `put`, canonicalizing the nearest existing ancestor directory and rejecting any symlink component along the path inside `base_dir` before creating directories or renaming the temp file), atomic temp-file write + rename on `put`, sidecar/computed ETag (SHA-256 hex) on `head`, prefix listing, and `ObjectStoreError::Unsupported` (or configurable local URL signer) on `presign_*`.
   - `S3ObjectStore` (`#[cfg(feature = "s3")]`): pure-Rust AWS SigV4 over `reqwest`, `hmac`, `sha2`, `hex`, `chrono`, `url` (deriving UTC timestamps via `chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())` so only `chrono`'s `std` feature is required) supporting path-style & virtual-hosted-style endpoints (S3, MinIO, R2), `put`, `get`, `head`, `delete`, `list` (XML `<Key>`/`<Size>`/`<ETag>` parsing), and `presign_get` / `presign_put` query-string signing.
2. **`rume-http::upload`**:
   - `MultipartUploadConfig { max_file_size_bytes: usize, max_files: usize, allowed_mime_types: Vec<String>, allowed_extensions: Vec<String> }` with safe defaults (e.g. 10 MiB max file size, 10 max files). Note: Axum's `DefaultBodyLimit` defaults to 2 MiB and rejects larger bodies before `FromRequest` runs; document and test pairing `ValidatedMultipart` with `axum::extract::DefaultBodyLimit::disable()` (or `RequestBodyLimitLayer`) on routes accepting files larger than 2 MiB.
   - `UploadedFile { field_name: String, file_name: String, sanitized_file_name: String, content_type: Option<String>, bytes: Vec<u8> }`.
   - `sanitize_filename(raw: &str) -> String`: strips directory separators (`/` and `\`), control chars, null bytes, and leading dots; falls back to `"upload.bin"` if empty.
   - `ValidatedMultipart` Axum `FromRequest` extractor (and `extract_multipart_files` helper using `MultipartUploadConfig`) that streams chunks, enforces `max_file_size_bytes` mid-stream (aborting before buffering oversized payloads), validates MIME type & extension allow-lists, and rejects violations with `AppError::BadRequest` / `AppError::Public` (`413 Payload Too Large` / `415 Unsupported Media Type`).

## Constraints honoured

- `#![forbid(unsafe_code)]` across `rume-data` and `rume-http`.
- `crates/data/Cargo.toml`: `S3ObjectStore` gated behind `s3` feature so default builds stay lightweight.
- `crates/foundation/src/error.rs`: multipart rejections return sanitized `AppError` RFC 9457 Problem Details.

## Named tests (bodies omitted)

- `crates/data/src/storage/local.rs`:
  - `local_fs_store_put_get_head_list_delete_roundtrip` — verifies full CRUD + prefix listing in `tempfile::tempdir()`.
  - `local_fs_store_rejects_path_traversal_and_invalid_keys` — verifies `../escape`, `/abs`, `a\\b`, `\0`, and symlinked ancestor/target paths inside `base_dir` are rejected with `ObjectStoreError::InvalidKey` / `ObjectStoreError::PermissionDenied`.
- `crates/data/src/storage/s3.rs`:
  - `s3_sigv4_presigned_urls_match_canonical_format` — verifies `X-Amz-Algorithm=AWS4-HMAC-SHA256`, `X-Amz-Credential`, `X-Amz-SignedHeaders`, and `X-Amz-Signature` determinism.
  - `s3_store_put_get_head_delete_against_mock_server` — verifies HTTP requests/headers against a local mock TCP/HTTP server (or `wiremock` in dev-deps / integration).
- `crates/http/src/upload.rs`:
  - `sanitize_filename_strips_traversal_and_control_chars` — verifies `../../etc/passwd`, `.hidden`, and empty names sanitize safely.
  - `multipart_extractor_enforces_size_mime_and_extension_limits` — verifies valid upload succeeds, oversized stream returns `413`, and disallowed MIME/extension returns `415`/`400`.

## Verification

```bash
cargo test -j 2 -p rume-data --lib --all-features
cargo test -j 2 -p rume-http --lib --all-features
cargo clippy -j 2 -p rume-data -p rume-http --all-targets --all-features -- -D warnings
```

Expected: all storage and multipart upload tests pass with zero warnings.

## Commit

Only when the user asks:

```text
feat(data,http): add ObjectStore (LocalFs + SigV4 S3) and ValidatedMultipart upload extractor
```

## Done-when

- [ ] `LocalFsObjectStore` and `S3ObjectStore` (`s3` feature) implement `ObjectStore` with path-traversal protection and SigV4 signing.
- [ ] `ValidatedMultipart` enforces streaming size, MIME, extension, and filename sanitization rules.
- [ ] All tests and clippy pass with zero warnings.
