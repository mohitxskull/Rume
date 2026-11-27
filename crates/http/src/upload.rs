//! Streaming multipart file upload extractor and validation.
//!
//! Note: Axum's `DefaultBodyLimit` defaults to 2 MiB and rejects larger
//! payloads before [`ValidatedMultipart`] runs. Routes accepting uploads larger
//! than 2 MiB should pair [`ValidatedMultipart`] with
//! `axum::extract::DefaultBodyLimit::disable()` (or `RequestBodyLimitLayer`).

use std::path::Path;

use axum::extract::{FromRequest, Multipart, Request};
use axum::http::StatusCode;
use rume_foundation::AppError;

const DEFAULT_MAX_FILE_SIZE_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_FILES: usize = 10;
const DEFAULT_MAX_TOTAL_BYTES: usize = 100 * 1024 * 1024;
const DEFAULT_MAX_FIELDS: usize = 64;

/// Configuration governing [`ValidatedMultipart`] and [`extract_multipart_files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartUploadConfig {
    pub max_file_size_bytes: usize,
    pub max_files: usize,
    /// Maximum cumulative bytes accepted across all multipart fields (file and non-file).
    pub max_total_bytes: usize,
    /// Maximum number of multipart fields (both file and non-file) accepted per request.
    pub max_fields: usize,
    pub allowed_mime_types: Vec<String>,
    pub allowed_extensions: Vec<String>,
}

impl Default for MultipartUploadConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: DEFAULT_MAX_FILE_SIZE_BYTES,
            max_files: DEFAULT_MAX_FILES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_fields: DEFAULT_MAX_FIELDS,
            allowed_mime_types: Vec::new(),
            allowed_extensions: Vec::new(),
        }
    }
}

/// A validated uploaded file extracted from a multipart request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedFile {
    pub field_name: String,
    pub file_name: String,
    pub sanitized_file_name: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// Axum [`FromRequest`] extractor that streams and validates multipart file uploads.
///
/// Reads an optional [`MultipartUploadConfig`] from request extensions, falling
/// back to [`MultipartUploadConfig::default()`].
#[derive(Debug, Clone)]
pub struct ValidatedMultipart {
    pub files: Vec<UploadedFile>,
}

impl<S> FromRequest<S> for ValidatedMultipart
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let config = req
            .extensions()
            .get::<MultipartUploadConfig>()
            .cloned()
            .unwrap_or_default();

        let multipart =
            Multipart::from_request(req, state)
                .await
                .map_err(|e| AppError::BadRequest {
                    code: "INVALID_MULTIPART",
                    message: format!("invalid multipart request: {e}"),
                })?;

        let files = extract_multipart_files(multipart, &config).await?;
        Ok(Self { files })
    }
}

/// Sanitizes an untrusted client filename.
///
/// Strips directory prefixes (`/` and `\`), control characters, null bytes,
/// and leading dots, falling back to `"upload.bin"` if the result is empty.
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        "upload.bin".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Streams and validates multipart file fields according to `config`.
pub async fn extract_multipart_files(
    mut multipart: Multipart,
    config: &MultipartUploadConfig,
) -> Result<Vec<UploadedFile>, AppError> {
    let mut files = Vec::new();
    let mut total_fields: usize = 0;
    let mut total_bytes: usize = 0;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest {
            code: "INVALID_MULTIPART",
            message: format!("failed to read multipart field: {e}"),
        })?
    {
        // Why: bound both field count and total bytes across ALL fields (file and non-file)
        // so an attacker cannot exhaust CPU/RAM with unbounded non-file form fields.
        total_fields = total_fields.saturating_add(1);
        if total_fields > config.max_fields {
            return Err(AppError::BadRequest {
                code: "TOO_MANY_FIELDS",
                message: format!("maximum of {} multipart fields exceeded", config.max_fields),
            });
        }

        let Some(raw_file_name) = field.file_name().map(ToString::to_string) else {
            while let Some(chunk) = field.chunk().await.map_err(|e| AppError::BadRequest {
                code: "INVALID_MULTIPART",
                message: format!("failed to read multipart chunk: {e}"),
            })? {
                if total_bytes.saturating_add(chunk.len()) > config.max_total_bytes {
                    return Err(AppError::Public {
                        status: StatusCode::PAYLOAD_TOO_LARGE,
                        code: "PAYLOAD_TOO_LARGE",
                        message: format!(
                            "multipart payload exceeds maximum total size of {} bytes",
                            config.max_total_bytes
                        ),
                    });
                }
                total_bytes = total_bytes.saturating_add(chunk.len());
            }
            continue;
        };

        if files.len() >= config.max_files {
            return Err(AppError::BadRequest {
                code: "TOO_MANY_FILES",
                message: format!("maximum of {} uploaded files exceeded", config.max_files),
            });
        }

        let field_name = field.name().unwrap_or("file").to_string();
        let content_type = field.content_type().map(ToString::to_string);
        let sanitized_file_name = sanitize_filename(&raw_file_name);

        validate_mime_type(content_type.as_deref(), &config.allowed_mime_types)?;
        validate_extension(&sanitized_file_name, &config.allowed_extensions)?;

        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|e| AppError::BadRequest {
            code: "INVALID_MULTIPART",
            message: format!("failed to read multipart chunk: {e}"),
        })? {
            if bytes.len().saturating_add(chunk.len()) > config.max_file_size_bytes {
                return Err(AppError::Public {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "PAYLOAD_TOO_LARGE",
                    message: format!(
                        "uploaded file exceeds maximum size of {} bytes",
                        config.max_file_size_bytes
                    ),
                });
            }
            if total_bytes.saturating_add(chunk.len()) > config.max_total_bytes {
                return Err(AppError::Public {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "PAYLOAD_TOO_LARGE",
                    message: format!(
                        "multipart payload exceeds maximum total size of {} bytes",
                        config.max_total_bytes
                    ),
                });
            }
            total_bytes = total_bytes.saturating_add(chunk.len());
            bytes.extend_from_slice(&chunk);
        }

        validate_sniffed_mime_type(&bytes, content_type.as_deref(), &config.allowed_mime_types)?;

        files.push(UploadedFile {
            field_name,
            file_name: raw_file_name,
            sanitized_file_name,
            content_type,
            bytes,
        });
    }

    Ok(files)
}

/// Sniffs the MIME type of `bytes` using `infer` for binary magic signatures (100+ formats)
/// with a text-based HTML/SVG/XML script detection fallback.
///
/// Returns `None` for unrecognized or plain-text payloads without a distinctive
/// signature.
pub fn sniff_mime_type(bytes: &[u8]) -> Option<&'static str> {
    // 1. Binary magic-byte signatures via `infer` (covers PNG, JPEG, GIF, WebP, PDF, ZIP, GZIP,
    //    and 100+ additional binary formats).
    if let Some(kind) = infer::get(bytes) {
        let mime = kind.mime_type();
        // If `infer` detects generic XML (`text/xml`), still refine to `image/svg+xml`
        // when an `<svg` root/element is present.
        if mime != "text/xml" {
            return Some(mime);
        }
    }

    // 2. Text / markup / script signatures: strip optional UTF-8 BOM and leading ASCII
    //    whitespace on raw bytes BEFORE bounding to 512 bytes, and decode lossily so
    //    invalid trailing UTF-8 (e.g. `\xff`) or >=512 leading spaces cannot bypass
    //    HTML/script/SVG/XML detection (`RC-05`).
    let mut trimmed = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    while let Some((&b, rest)) = trimmed.split_first() {
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c') {
            trimmed = rest;
        } else {
            break;
        }
    }
    if trimmed.is_empty() {
        return None;
    }
    let prefix_len = trimmed.len().min(512);
    let prefix = String::from_utf8_lossy(&trimmed[..prefix_len]);
    let lower = prefix.to_ascii_lowercase();

    let starts_with_tag = |tag: &str| -> bool {
        lower.strip_prefix(tag).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, '>' | '/'))
        })
    };

    if lower.starts_with("<!doctype html")
        || starts_with_tag("<html")
        || starts_with_tag("<script")
        || starts_with_tag("<head")
        || starts_with_tag("<body")
        || starts_with_tag("<iframe")
        || starts_with_tag("<object")
        || starts_with_tag("<embed")
        || starts_with_tag("<style")
    {
        return Some("text/html");
    }
    if starts_with_tag("<svg") {
        return Some("image/svg+xml");
    }
    if lower.starts_with("<?xml") {
        if lower.contains("<svg") {
            return Some("image/svg+xml");
        }
        return Some("application/xml");
    }

    None
}

fn requires_magic_signature(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/tiff"
            | "image/avif"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
            | "application/pdf"
            | "application/zip"
            | "application/gzip"
            | "application/x-tar"
            | "application/x-rar-compressed"
            | "application/vnd.rar"
            | "application/x-7z-compressed"
            | "application/x-bzip2"
            | "application/x-xz"
            | "application/zstd"
            | "application/wasm"
            | "video/mp4"
            | "video/webm"
            | "video/x-matroska"
            | "video/quicktime"
            | "video/x-msvideo"
            | "audio/mpeg"
            | "audio/ogg"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/flac"
            | "audio/x-flac"
            | "audio/aac"
            | "font/woff"
            | "font/woff2"
            | "font/ttf"
            | "font/otf"
    ) || mime.starts_with("application/vnd.openxmlformats-officedocument.")
}

fn mime_types_equivalent(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    matches!(
        (
            a.to_ascii_lowercase().as_str(),
            b.to_ascii_lowercase().as_str()
        ),
        ("text/xml", "application/xml")
            | ("application/xml", "text/xml")
            | ("image/x-icon", "image/vnd.microsoft.icon")
            | ("image/vnd.microsoft.icon", "image/x-icon")
            | ("audio/wav", "audio/x-wav")
            | ("audio/x-wav", "audio/wav")
            | ("audio/flac", "audio/x-flac")
            | ("audio/x-flac", "audio/flac")
            | ("application/x-rar-compressed", "application/vnd.rar")
            | ("application/vnd.rar", "application/x-rar-compressed")
    )
}

fn validate_sniffed_mime_type(
    bytes: &[u8],
    content_type: Option<&str>,
    allowed_mime_types: &[String],
) -> Result<(), AppError> {
    if allowed_mime_types.is_empty() {
        return Ok(());
    }

    let declared = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase());

    // Why: client-supplied `Content-Type` headers are untrusted; sniff magic bytes
    // and markup signatures so HTML/script payloads disguised as `image/png` (or
    // binary mismatches) are rejected with 415 Unsupported Media Type (`RC-16`, `RC-05`).
    match sniff_mime_type(bytes) {
        Some(detected) => {
            let detected_allowed = allowed_mime_types
                .iter()
                .any(|a| mime_types_equivalent(a, detected));
            let matches_declared = declared
                .as_deref()
                .is_none_or(|d| mime_types_equivalent(d, detected));
            if !detected_allowed || !matches_declared {
                return Err(AppError::Public {
                    status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    code: "UNSUPPORTED_MEDIA_TYPE",
                    message: format!(
                        "uploaded file content (detected {detected}) does not match allowed MIME types"
                    ),
                });
            }
        }
        None => {
            if let Some(ref d) = declared
                && requires_magic_signature(d)
            {
                return Err(AppError::Public {
                    status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    code: "UNSUPPORTED_MEDIA_TYPE",
                    message: format!("uploaded file content does not match declared MIME type {d}"),
                });
            }
        }
    }

    Ok(())
}

fn validate_mime_type(
    content_type: Option<&str>,
    allowed_mime_types: &[String],
) -> Result<(), AppError> {
    if allowed_mime_types.is_empty() {
        return Ok(());
    }

    let essence = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase());

    let allowed = essence
        .as_deref()
        .is_some_and(|e| allowed_mime_types.iter().any(|a| a.eq_ignore_ascii_case(e)));

    if !allowed {
        return Err(AppError::Public {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "UNSUPPORTED_MEDIA_TYPE",
            message: "uploaded file MIME type is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_extension(file_name: &str, allowed_extensions: &[String]) -> Result<(), AppError> {
    if allowed_extensions.is_empty() {
        return Ok(());
    }

    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    let allowed = ext.as_deref().is_some_and(|e| {
        allowed_extensions
            .iter()
            .any(|a| a.trim_start_matches('.').eq_ignore_ascii_case(e))
    });

    if !allowed {
        return Err(AppError::BadRequest {
            code: "UNSUPPORTED_FILE_EXTENSION",
            message: "uploaded file extension is not allowed".to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::post;
    use axum::{Extension, Json, Router};
    use tower::ServiceExt;

    #[test]
    fn sanitize_filename_strips_traversal_and_control_chars() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename(r"..\..\secret.txt"), "secret.txt");
        assert_eq!(sanitize_filename(".hidden"), "hidden");
        assert_eq!(sanitize_filename(".."), "upload.bin");
        assert_eq!(sanitize_filename(""), "upload.bin");
        assert_eq!(sanitize_filename("a\x00b\nc.txt"), "abc.txt");
    }

    fn build_multipart_body(
        boundary: &str,
        name: &str,
        filename: &str,
        content_type: &str,
        data: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }

    #[tokio::test]
    async fn multipart_extractor_enforces_size_mime_and_extension_limits() {
        let config = MultipartUploadConfig {
            max_file_size_bytes: 16,
            max_files: 2,
            allowed_mime_types: vec!["text/plain".to_string()],
            allowed_extensions: vec!["txt".to_string()],
            ..Default::default()
        };

        let app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len(), "name": files[0].sanitized_file_name }))
                }),
            )
            .layer(Extension(config));

        let boundary = "----rume-test-boundary";

        // Valid upload
        let valid_body =
            build_multipart_body(boundary, "file", "../hello.txt", "text/plain", b"hello");
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(valid_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Oversized upload -> 413
        let big_body = build_multipart_body(boundary, "file", "big.txt", "text/plain", &[b'x'; 32]);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(big_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // Disallowed MIME -> 415
        let bad_mime_body = build_multipart_body(
            boundary,
            "file",
            "doc.txt",
            "application/octet-stream",
            b"hi",
        );
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(bad_mime_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // Disallowed extension -> 400 or 415
        let bad_ext_body =
            build_multipart_body(boundary, "file", "script.exe", "text/plain", b"hi");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(bad_ext_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::BAD_REQUEST
                || resp.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    #[tokio::test]
    async fn multipart_extractor_accepts_large_upload_with_default_body_limit_disabled() {
        let config = MultipartUploadConfig {
            max_file_size_bytes: 3 * 1024 * 1024,
            max_files: 1,
            allowed_mime_types: vec![],
            allowed_extensions: vec![],
            ..Default::default()
        };

        let app = Router::new()
            .route(
                "/upload-large",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "size": files[0].bytes.len() }))
                }),
            )
            .layer(axum::extract::DefaultBodyLimit::disable())
            .layer(Extension(config));

        let boundary = "----rume-large-boundary";
        let payload = vec![b'a'; 2_500_000];
        let body = build_multipart_body(
            boundary,
            "file",
            "large.bin",
            "application/octet-stream",
            &payload,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload-large")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn multipart_extractor_enforces_max_fields_and_max_total_bytes_on_non_file_fields() {
        let config = MultipartUploadConfig {
            max_file_size_bytes: 64,
            max_files: 4,
            max_total_bytes: 32,
            max_fields: 3,
            allowed_mime_types: vec![],
            allowed_extensions: vec![],
        };

        let app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(config));

        let boundary = "----rume-bounds-boundary";

        // 4 non-file fields exceeds max_fields = 3 -> 400 Bad Request
        let mut too_many_fields = Vec::new();
        for i in 0..4 {
            too_many_fields.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"f{i}\"\r\n\r\nhi\r\n"
                )
                .as_bytes(),
            );
        }
        too_many_fields.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(too_many_fields))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Single non-file field of 40 bytes exceeds max_total_bytes = 32 -> 413
        let mut oversized_non_file = Vec::new();
        oversized_non_file.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\n")
                .as_bytes(),
        );
        oversized_non_file.extend_from_slice(&[b'x'; 40]);
        oversized_non_file.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(oversized_non_file))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn multipart_extractor_rejects_html_payload_disguised_as_image_png() {
        let config = MultipartUploadConfig {
            max_file_size_bytes: 1024,
            max_files: 1,
            allowed_mime_types: vec!["image/png".to_string()],
            allowed_extensions: vec!["png".to_string()],
            ..Default::default()
        };

        let app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(config));

        let boundary = "----rume-sniff-boundary";

        // HTML/script payload pretending to be image/png -> 415 Unsupported Media Type
        let spoofed_body = build_multipart_body(
            boundary,
            "file",
            "evil.png",
            "image/png",
            b"<html><script>alert(1)</script></html>",
        );
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(spoofed_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // Genuine PNG magic header -> 200 OK
        let valid_png = build_multipart_body(
            boundary,
            "file",
            "ok.png",
            "image/png",
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(valid_png))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn multipart_sniffing_rejects_invalid_utf8_and_padded_html_and_accepts_text_xml() {
        let img_config = MultipartUploadConfig {
            max_file_size_bytes: 4096,
            max_files: 1,
            allowed_mime_types: vec!["image/png".to_string()],
            allowed_extensions: vec!["png".to_string()],
            ..Default::default()
        };

        let img_app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(img_config));

        let boundary = "----rume-rc05-boundary";

        // 1. HTML script with invalid UTF-8 trailing byte `\xff` pretending to be image/png -> 415
        let invalid_utf8_xss = build_multipart_body(
            boundary,
            "file",
            "evil.png",
            "image/png",
            b"<script>alert(1)</script>\xff",
        );
        let resp = img_app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(invalid_utf8_xss))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // 2. >=512 leading spaces before `<html><body>xss</body></html>` -> 415
        let mut padded_html = vec![b' '; 512];
        padded_html.extend_from_slice(b"<html><body>xss</body></html>");
        let padded_body =
            build_multipart_body(boundary, "file", "padded.png", "image/png", &padded_html);
        let resp = img_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(padded_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // 3. Legitimate XML uploaded as `text/xml` when `text/xml` is allowed -> 200 OK
        let xml_config = MultipartUploadConfig {
            max_file_size_bytes: 4096,
            max_files: 1,
            allowed_mime_types: vec!["text/xml".to_string()],
            allowed_extensions: vec!["xml".to_string()],
            ..Default::default()
        };
        let xml_app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(xml_config));
        let xml_body = build_multipart_body(
            boundary,
            "file",
            "feed.xml",
            "text/xml",
            b"<?xml version=\"1.0\"?><root/>",
        );
        let resp = xml_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(xml_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 4. Form-feed (`\x0c`) leading whitespace before `<script>` pretending to be image/png -> 415
        let ff_config = MultipartUploadConfig {
            max_file_size_bytes: 4096,
            max_files: 1,
            allowed_mime_types: vec!["image/png".to_string()],
            allowed_extensions: vec!["png".to_string()],
            ..Default::default()
        };
        let ff_app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(ff_config));
        let ff_body = build_multipart_body(
            boundary,
            "file",
            "ff.png",
            "image/png",
            b"\x0c<script>alert(1)</script>",
        );
        let resp = ff_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(ff_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // 5. Genuine WAV uploaded with `audio/wav` when `infer` returns `audio/x-wav` -> 200 OK
        let wav_config = MultipartUploadConfig {
            max_file_size_bytes: 4096,
            max_files: 1,
            allowed_mime_types: vec!["audio/wav".to_string()],
            allowed_extensions: vec!["wav".to_string()],
            ..Default::default()
        };
        let wav_app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(wav_config));
        let wav_body = build_multipart_body(
            boundary,
            "file",
            "clip.wav",
            "audio/wav",
            b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00",
        );
        let resp = wav_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(wav_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
