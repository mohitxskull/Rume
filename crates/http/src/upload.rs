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

/// Maximum sanitized filename byte length (POSIX/NTFS component limit).
const MAX_FILENAME_BYTES: usize = 255;

fn is_windows_reserved_stem(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Sanitizes an untrusted client filename.
///
/// Strips directory prefixes (`/` and `\`), control characters, null bytes,
/// NTFS stream colons (`:`), and leading/trailing dots and whitespace, prefixes
/// Windows reserved device names (`CON`, `NUL`, etc.), caps length to 255 UTF-8
/// bytes, and falls back to `"upload.bin"` if the result is empty.
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .collect();

    let mut trimmed = cleaned.as_str();
    loop {
        let next = trimmed.trim().trim_matches('.');
        if next.len() == trimmed.len() {
            break;
        }
        trimmed = next;
    }

    if trimmed.is_empty() {
        return "upload.bin".to_string();
    }

    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    let mut out = if is_windows_reserved_stem(stem) {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    };

    if out.len() > MAX_FILENAME_BYTES {
        // Why: truncate ONLY the stem while preserving the trailing `.ext` so an attacker
        // cannot smuggle a disallowed extension (e.g. `a...a.png.html` -> `a...a.png`) past
        // `validate_extension` via 255-byte truncation (`P2-09`).
        out = if let Some((raw_stem, ext)) = out.rsplit_once('.')
            && !raw_stem.is_empty()
            && !ext.is_empty()
            && ext.len() + 1 < MAX_FILENAME_BYTES
        {
            let max_stem = MAX_FILENAME_BYTES - ext.len() - 1;
            let truncated_stem = truncate_utf8_bytes(raw_stem, max_stem)
                .trim_end_matches('.')
                .trim_end();
            let safe_stem = if truncated_stem.is_empty() {
                "upload"
            } else {
                truncated_stem
            };
            format!("{safe_stem}.{ext}")
        } else {
            truncate_utf8_bytes(&out, MAX_FILENAME_BYTES)
                .trim_end_matches('.')
                .trim_end()
                .to_string()
        };
        if out.is_empty() {
            return "upload.bin".to_string();
        }
    }

    out
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

        validate_sniffed_mime_type(
            &bytes,
            content_type.as_deref(),
            &sanitized_file_name,
            &config.allowed_mime_types,
            &config.allowed_extensions,
        )?;

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

/// Maximum bytes inspected during text/markup MIME sniffing (8 KiB).
const TEXT_SNIFF_WINDOW_BYTES: usize = 8192;

/// Maximum raw bytes decoded when normalizing UTF-16 BOM payloads for markup sniffing.
const MAX_UTF16_SNIFF_BYTES: usize = 256 * 1024;

fn decode_utf16_bom(bytes: &[u8]) -> Option<Vec<u8>> {
    let (le, body) = if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        (true, rest)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        (false, rest)
    } else {
        return None;
    };
    let bounded = &body[..body.len().min(MAX_UTF16_SNIFF_BYTES)];
    let units: Vec<u16> = bounded
        .chunks_exact(2)
        .map(|c| {
            if le {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    Some(String::from_utf16_lossy(&units).into_bytes())
}

/// Skips a `<!DOCTYPE ...>` declaration including any `[ ... ]` internal subset containing
/// nested `<!ENTITY ...>` / `<!-- ... -->` declarations (`P2-09`).
fn skip_doctype_declaration(slice: &[u8]) -> Option<&[u8]> {
    if slice.len() < 9 || !slice[..9].eq_ignore_ascii_case(b"<!doctype") {
        return None;
    }
    let mut i = 9;
    let mut bracket_depth: usize = 0;
    let mut quote: Option<u8> = None;
    while i < slice.len() {
        let b = slice[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if slice[i..].starts_with(b"<!--") {
            if let Some(end_rel) = slice[i + 4..].windows(3).position(|w| w == b"-->") {
                i += 4 + end_rel + 3;
                continue;
            }
            return None;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b'[' => bracket_depth = bracket_depth.saturating_add(1),
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'>' if bracket_depth == 0 => return Some(&slice[i + 1..]),
            _ => {}
        }
        i += 1;
    }
    None
}

struct StrippedProlog<'a> {
    rest: &'a [u8],
    saw_xml_prolog: bool,
    saw_html_doctype: bool,
}

fn strip_leading_ws_and_prolog(mut slice: &[u8]) -> StrippedProlog<'_> {
    let mut saw_xml_prolog = false;
    let mut saw_html_doctype = false;
    loop {
        while let Some((&b, rest)) = slice.split_first() {
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c') {
                slice = rest;
            } else {
                break;
            }
        }
        if let Some(after_open) = slice.strip_prefix(b"<!--")
            && let Some(end_idx) = after_open.windows(3).position(|w| w == b"-->")
        {
            slice = &after_open[end_idx + 3..];
            continue;
        }
        if let Some(after_pi) = slice.strip_prefix(b"<?")
            && let Some(end_idx) = after_pi.windows(2).position(|w| w == b"?>")
        {
            if slice.len() >= 5 && slice[..5].eq_ignore_ascii_case(b"<?xml") {
                saw_xml_prolog = true;
            }
            slice = &after_pi[end_idx + 2..];
            continue;
        }
        if slice.len() >= 9 && slice[..9].eq_ignore_ascii_case(b"<!doctype") {
            let after_kw = &slice[9..];
            let ws_skipped = after_kw
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c'))
                .map(|idx| &after_kw[idx..])
                .unwrap_or(&[]);
            if ws_skipped.len() >= 4
                && ws_skipped[..4].eq_ignore_ascii_case(b"html")
                && ws_skipped
                    .get(4)
                    .is_none_or(|b| b.is_ascii_whitespace() || *b == b'>')
            {
                saw_html_doctype = true;
            }
            if let Some(after_doctype) = skip_doctype_declaration(slice) {
                slice = after_doctype;
                continue;
            }
        }
        break;
    }
    StrippedProlog {
        rest: slice,
        saw_xml_prolog,
        saw_html_doctype,
    }
}

/// Sniffs the MIME type of `bytes` using `infer` for binary magic signatures (100+ formats)
/// with a text-based HTML/SVG/XML script detection fallback.
///
/// Returns `None` for unrecognized or plain-text payloads without a distinctive
/// signature.
pub fn sniff_mime_type(bytes: &[u8]) -> Option<&'static str> {
    // 1. Binary magic-byte signatures via `infer` (covers PNG, JPEG, GIF, WebP, PDF, ZIP, GZIP,
    //    and 100+ additional binary formats).
    let mut infer_html_fallback = None;
    if let Some(kind) = infer::get(bytes) {
        let mime = kind.mime_type();
        // Why: `infer` classifies `<?xml` as `text/xml` and leading `<!--` comments as
        // `text/html` without inspecting what follows, which misclassifies commented SVGs
        // (`<!-- ... --><svg>`). Let the markup sniffer refine both (`RC-16`).
        if mime != "text/xml" && mime != "text/html" {
            return Some(mime);
        }
        if mime == "text/html" {
            infer_html_fallback = Some(mime);
        }
    }

    // 2. Text / markup / script signatures: decode optional UTF-16LE/BE BOM or strip UTF-8 BOM,
    //    then strip leading whitespace, HTML/XML comments (`<!-- ... -->`), processing
    //    instructions (`<?...?>`), and `<!DOCTYPE ...>` declarations BEFORE bounding to 8 KiB
    //    so padded prologs/DOCTYPE subsets or UTF-16 payloads cannot bypass detection (`RC-05`, `RC-16`, `P2-09`).
    let utf16_decoded = decode_utf16_bom(bytes);
    let after_bom: &[u8] = if let Some(ref decoded) = utf16_decoded {
        decoded.as_slice()
    } else {
        bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
    };

    let prolog = strip_leading_ws_and_prolog(after_bom);
    if prolog.rest.is_empty() {
        if prolog.saw_html_doctype {
            return Some("text/html");
        }
        if prolog.saw_xml_prolog {
            return Some("application/xml");
        }
        return infer_html_fallback;
    }

    let prefix_len = prolog.rest.len().min(TEXT_SNIFF_WINDOW_BYTES);
    let prefix = String::from_utf8_lossy(&prolog.rest[..prefix_len]);
    let lower = prefix.to_ascii_lowercase();

    let starts_with_tag = |s: &str, tag: &str| -> bool {
        s.strip_prefix(tag).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, '>' | '/'))
        })
    };

    if starts_with_tag(&lower, "<svg") {
        return Some("image/svg+xml");
    }
    if lower.starts_with("<!doctype html")
        || prolog.saw_html_doctype
        || starts_with_tag(&lower, "<html")
        || starts_with_tag(&lower, "<script")
        || starts_with_tag(&lower, "<head")
        || starts_with_tag(&lower, "<body")
        || starts_with_tag(&lower, "<iframe")
        || starts_with_tag(&lower, "<object")
        || starts_with_tag(&lower, "<embed")
        || starts_with_tag(&lower, "<style")
    {
        return Some("text/html");
    }
    if prolog.saw_xml_prolog || lower.starts_with("<?xml") {
        if lower.contains("<svg") {
            return Some("image/svg+xml");
        }
        if lower.contains("<html") || lower.contains("<script") {
            return Some("text/html");
        }
        return Some("application/xml");
    }

    infer_html_fallback
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

fn expected_mime_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "avif" => Some("image/avif"),
        "ico" => Some("image/x-icon"),
        "svg" => Some("image/svg+xml"),
        "pdf" => Some("application/pdf"),
        "zip" => Some("application/zip"),
        "gz" | "gzip" => Some("application/gzip"),
        "tar" => Some("application/x-tar"),
        "rar" => Some("application/x-rar-compressed"),
        "7z" => Some("application/x-7z-compressed"),
        "bz2" => Some("application/x-bzip2"),
        "xz" => Some("application/x-xz"),
        "zst" => Some("application/zstd"),
        "wasm" => Some("application/wasm"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mkv" => Some("video/x-matroska"),
        "mov" => Some("video/quicktime"),
        "avi" => Some("video/x-msvideo"),
        "mp3" => Some("audio/mpeg"),
        "ogg" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "flac" => Some("audio/flac"),
        "aac" => Some("audio/aac"),
        "woff" => Some("font/woff"),
        "woff2" => Some("font/woff2"),
        "ttf" => Some("font/ttf"),
        "otf" => Some("font/otf"),
        "xml" => Some("application/xml"),
        "html" | "htm" => Some("text/html"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        _ => None,
    }
}

fn validate_sniffed_mime_type(
    bytes: &[u8],
    content_type: Option<&str>,
    file_name: &str,
    allowed_mime_types: &[String],
    allowed_extensions: &[String],
) -> Result<(), AppError> {
    if allowed_mime_types.is_empty() && allowed_extensions.is_empty() {
        return Ok(());
    }

    let declared = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase());
    let sniffed = sniff_mime_type(bytes);

    // Why: client-supplied `Content-Type` headers are untrusted; sniff magic bytes
    // and markup signatures so HTML/script payloads disguised as `image/png` (or
    // binary mismatches) are rejected with 415 Unsupported Media Type (`RC-16`, `RC-05`).
    if !allowed_mime_types.is_empty() {
        match sniffed {
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
                        message: format!(
                            "uploaded file content does not match declared MIME type {d}"
                        ),
                    });
                }
            }
        }
    }

    // Why: configurations that restrict only `allowed_extensions` (leaving `allowed_mime_types`
    // empty) must still cross-check sniffed content against the file extension so HTML or
    // plain text uploaded as `avatar.png` is rejected (`RC-16`).
    if !allowed_extensions.is_empty() {
        let ext = Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let expected_mime = ext.as_deref().and_then(expected_mime_for_extension);

        match (expected_mime, sniffed) {
            (Some(expected), Some(detected)) if !mime_types_equivalent(expected, detected) => {
                return Err(AppError::Public {
                    status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    code: "UNSUPPORTED_MEDIA_TYPE",
                    message: format!(
                        "uploaded file content (detected {detected}) does not match file extension"
                    ),
                });
            }
            (Some(expected), None) if requires_magic_signature(expected) => {
                return Err(AppError::Public {
                    status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    code: "UNSUPPORTED_MEDIA_TYPE",
                    message: format!(
                        "uploaded file content does not match expected MIME type {expected} for extension"
                    ),
                });
            }
            // Why: unmapped/text extensions (`txt`, `csv`, `json`, `md`, etc.) have no
            // magic-byte mapping in `expected_mime_for_extension`; detecting ANY binary or
            // markup signature (ELF, PE, PDF, ZIP, HTML, SVG, XML, etc.) means the payload
            // is being smuggled under a text extension (`P2-09`).
            (None, Some(detected)) => {
                return Err(AppError::Public {
                    status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    code: "UNSUPPORTED_MEDIA_TYPE",
                    message: format!(
                        "uploaded file content (detected {detected}) does not match file extension"
                    ),
                });
            }
            _ => {}
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
        assert_eq!(sanitize_filename(". .."), "upload.bin");
        assert_eq!(sanitize_filename(""), "upload.bin");
        assert_eq!(sanitize_filename("a\x00b\nc.txt"), "abc.txt");
        assert_eq!(
            sanitize_filename("file.txt:zone.identifier"),
            "file.txtzone.identifier"
        );
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("LPT1.log"), "_LPT1.log");
        let long = "a".repeat(400);
        assert_eq!(sanitize_filename(&long).len(), 255);
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

    #[tokio::test]
    async fn allowed_extensions_only_cross_checks_sniffed_mime_and_long_comment_svg() {
        // 1. >512B comment before `<svg>` must still be detected as `image/svg+xml` (not `application/xml`)
        let long_comment = format!("<!-- {} -->", "x".repeat(1024));
        let svg_after_comment =
            format!("{long_comment}<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
        assert_eq!(
            sniff_mime_type(svg_after_comment.as_bytes()),
            Some("image/svg+xml")
        );

        let xml_then_long_comment_svg = format!(
            "<?xml version=\"1.0\"?>{long_comment}<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
        );
        assert_eq!(
            sniff_mime_type(xml_then_long_comment_svg.as_bytes()),
            Some("image/svg+xml")
        );

        // 2. `allowed_extensions: ["png"]` with empty `allowed_mime_types` rejects HTML and plain text
        let ext_only_config = MultipartUploadConfig {
            max_file_size_bytes: 4096,
            max_files: 1,
            allowed_mime_types: vec![],
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
            .layer(Extension(ext_only_config));
        let boundary = "----rume-rc16-boundary";

        let html_as_png = build_multipart_body(
            boundary,
            "file",
            "avatar.png",
            "text/html",
            b"<html><script>alert(1)</script></html>",
        );
        let resp_html = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(html_as_png))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_html.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let text_as_png = build_multipart_body(
            boundary,
            "file",
            "avatar.png",
            "text/plain",
            b"definitely not a png",
        );
        let resp_text = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(text_as_png))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_text.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // Genuine PNG still succeeds
        let ok_png = build_multipart_body(
            boundary,
            "file",
            "avatar.png",
            "image/png",
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
        );
        let resp_ok = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(ok_png))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_ok.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn p2_09_upload_hardening_preserves_ext_blocks_text_smuggling_and_sniffs_doctype_utf16_svg()
     {
        // 1. 255-byte truncation must preserve the real trailing extension (`.html`), not mutate it into `.png`.
        let crafted = format!("{}.png.html", "a".repeat(251));
        let sanitized = sanitize_filename(&crafted);
        assert!(sanitized.len() <= MAX_FILENAME_BYTES);
        assert!(
            sanitized.ends_with(".html"),
            "truncation must preserve `.html` extension, got '{sanitized}'"
        );
        assert!(
            validate_extension(&sanitized, &["png".to_string()]).is_err(),
            "truncated `.png.html` must still be rejected when only `png` is allowed"
        );

        // 2. `allowed_extensions: ["txt"]` with empty `allowed_mime_types` rejects binary/PDF/ELF smuggling.
        let txt_config = MultipartUploadConfig {
            max_file_size_bytes: 4096,
            max_files: 1,
            allowed_mime_types: vec![],
            allowed_extensions: vec!["txt".to_string()],
            ..Default::default()
        };
        let app = Router::new()
            .route(
                "/upload",
                post(|ValidatedMultipart { files }| async move {
                    Json(serde_json::json!({ "count": files.len() }))
                }),
            )
            .layer(Extension(txt_config));
        let boundary = "----rume-p209-boundary";

        // PDF smuggled as notes.txt -> 415
        let pdf_as_txt = build_multipart_body(
            boundary,
            "file",
            "notes.txt",
            "text/plain",
            b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n",
        );
        let resp_pdf = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(pdf_as_txt))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_pdf.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // ELF binary smuggled as notes.txt -> 415
        let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        elf.resize(64, 0);
        let elf_as_txt = build_multipart_body(boundary, "file", "notes.txt", "text/plain", &elf);
        let resp_elf = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(elf_as_txt))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_elf.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        // Genuine plain text still succeeds -> 200 OK
        let ok_txt = build_multipart_body(
            boundary,
            "file",
            "notes.txt",
            "text/plain",
            b"plain text notes",
        );
        let resp_ok = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(ok_txt))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_ok.status(), StatusCode::OK);

        // 3. `<!DOCTYPE svg [<!-- 9000 bytes -->]><svg/>` and UTF-16LE/BE BOM SVGs are detected.
        let padded_doctype_svg = format!(
            "<?xml version=\"1.0\"?><!DOCTYPE svg [<!-- {} -->]><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            "x".repeat(9000)
        );
        assert_eq!(
            sniff_mime_type(padded_doctype_svg.as_bytes()),
            Some("image/svg+xml")
        );

        let mut utf16le_svg = vec![0xFF, 0xFE];
        for unit in "<svg xmlns=\"http://www.w3.org/2000/svg\"/>".encode_utf16() {
            utf16le_svg.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(sniff_mime_type(&utf16le_svg), Some("image/svg+xml"));

        let mut utf16be_html = vec![0xFE, 0xFF];
        for unit in "<script>alert(1)</script>".encode_utf16() {
            utf16be_html.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(sniff_mime_type(&utf16be_html), Some("text/html"));
    }
}
