//! Pure-Rust SigV4 S3-compatible object store backend.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroizing;

use crate::storage::{
    BoxFuture, ObjectKey, ObjectMeta, ObjectStore, ObjectStoreError, PresignedUrl, PutObjectOptions,
};

type HmacSha256 = Hmac<Sha256>;

/// Default TCP connect timeout for [`S3ObjectStore`] HTTP requests (10 seconds).
pub const DEFAULT_S3_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default end-to-end request timeout for [`S3ObjectStore`] HTTP requests (60 seconds).
pub const DEFAULT_S3_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Default maximum number of `ListObjectsV2` pages fetched in a single `list()` call.
pub const DEFAULT_S3_MAX_LIST_PAGES: usize = 10_000;
/// Default maximum object body size accepted by `get()` (100 MiB).
pub const DEFAULT_S3_MAX_GET_BYTES: usize = 100 * 1024 * 1024;
/// Maximum AWS SigV4 presigned URL lifetime (7 days in seconds).
const MAX_PRESIGN_EXPIRES_SECS: u64 = 604_800;

/// Configuration for [`S3ObjectStore`].
#[derive(Clone, Default)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: SecretString,
    pub session_token: Option<SecretString>,
    pub path_style: bool,
    /// Optional TCP connect timeout (defaults to [`DEFAULT_S3_CONNECT_TIMEOUT`]).
    pub connect_timeout: Option<Duration>,
    /// Optional per-request timeout (defaults to [`DEFAULT_S3_REQUEST_TIMEOUT`]).
    pub request_timeout: Option<Duration>,
    /// Optional maximum pages fetched in `list()` (defaults to [`DEFAULT_S3_MAX_LIST_PAGES`]).
    pub max_list_pages: Option<usize>,
    /// Optional maximum bytes buffered in `get()` (defaults to [`DEFAULT_S3_MAX_GET_BYTES`]).
    pub max_get_bytes: Option<usize>,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("path_style", &self.path_style)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_list_pages", &self.max_list_pages)
            .field("max_get_bytes", &self.max_get_bytes)
            .finish()
    }
}

struct SigningRequest<'a> {
    method: &'a str,
    host: &'a str,
    canonical_uri: &'a str,
    canonical_query: &'a str,
    payload: &'a [u8],
    content_type: Option<&'a str>,
    now: DateTime<Utc>,
}

/// Pure-Rust AWS SigV4 S3-compatible [`ObjectStore`] implementation.
#[derive(Debug, Clone)]
pub struct S3ObjectStore {
    config: S3Config,
    base_url: Url,
    client: Client,
}

impl S3ObjectStore {
    /// Creates a new [`S3ObjectStore`] from `config`.
    pub fn new(config: S3Config) -> Result<Self, ObjectStoreError> {
        if config.bucket.is_empty()
            || config.region.is_empty()
            || config.access_key_id.is_empty()
            || config.secret_access_key.expose_secret().is_empty()
        {
            return Err(ObjectStoreError::Configuration(
                "S3 bucket, region, access_key_id, and secret_access_key must be non-empty"
                    .to_string(),
            ));
        }

        // Why: strict DNS-label allowlist (`[a-z0-9.-]`, no leading/trailing `.`/`-`, no `..`)
        // prevents userinfo (`user:pass@`), port (`:`), path, and query injection into
        // virtual-hosted (`https://{bucket}.{base_host}`) and path-style S3 URLs (`P2-22`).
        let bucket = config.bucket.as_str();
        let valid_bucket = !bucket.starts_with('.')
            && !bucket.ends_with('.')
            && !bucket.starts_with('-')
            && !bucket.ends_with('-')
            && !bucket.contains("..")
            && bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
        if !valid_bucket {
            return Err(ObjectStoreError::Configuration(format!(
                "invalid S3 bucket name '{}'",
                config.bucket
            )));
        }

        let base_url = Url::parse(&config.endpoint).map_err(|e| {
            ObjectStoreError::Configuration(format!("invalid S3 endpoint URL: {e}"))
        })?;
        if base_url.host_str().is_none() {
            return Err(ObjectStoreError::Configuration(
                "S3 endpoint URL must include a host".to_string(),
            ));
        }

        let connect_timeout = config.connect_timeout.unwrap_or(DEFAULT_S3_CONNECT_TIMEOUT);
        let request_timeout = config.request_timeout.unwrap_or(DEFAULT_S3_REQUEST_TIMEOUT);
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|e| {
                ObjectStoreError::Configuration(format!("failed to build HTTP client: {e}"))
            })?;

        Ok(Self {
            config,
            base_url,
            client,
        })
    }

    fn host_and_path(&self, key: Option<&str>) -> (String, String, String) {
        let base_host = self.base_url.host_str().unwrap_or_default();
        let port_suffix = self
            .base_url
            .port()
            .map(|p| format!(":{p}"))
            .unwrap_or_default();

        let base_path = self.base_url.path().trim_end_matches('/');
        let encoded_key = key.map(encode_s3_key).unwrap_or_default();

        if self.config.path_style {
            let host = format!("{base_host}{port_suffix}");
            let uri_path = if encoded_key.is_empty() {
                format!("{base_path}/{}", self.config.bucket)
            } else {
                format!("{base_path}/{}/{encoded_key}", self.config.bucket)
            };
            let full_url = format!("{}://{}{}", self.base_url.scheme(), host, uri_path);
            (host, uri_path, full_url)
        } else {
            let host = format!("{}.{base_host}{port_suffix}", self.config.bucket);
            let uri_path = if encoded_key.is_empty() {
                format!("{base_path}/")
            } else {
                format!("{base_path}/{encoded_key}")
            };
            let full_url = format!("{}://{}{}", self.base_url.scheme(), host, uri_path);
            (host, uri_path, full_url)
        }
    }

    fn signing_key(&self, date_stamp: &str) -> [u8; 32] {
        let k_secret = Zeroizing::new(format!(
            "AWS4{}",
            self.config.secret_access_key.expose_secret()
        ));
        let k_date = hmac_sha256(k_secret.as_bytes(), date_stamp.as_bytes());
        let k_region = hmac_sha256(&k_date, self.config.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        hmac_sha256(&k_service, b"aws4_request")
    }

    fn build_signed_headers(
        &self,
        req: &SigningRequest<'_>,
    ) -> Result<HeaderMap, ObjectStoreError> {
        let amz_date = req.now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = req.now.format("%Y%m%d").to_string();
        let payload_hash = hex::encode(Sha256::digest(req.payload));
        let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", self.config.region);

        let mut canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n",
            req.host
        );
        let mut signed_headers = String::from("host;x-amz-content-sha256;x-amz-date");

        if let Some(token) = &self.config.session_token {
            canonical_headers
                .push_str(&format!("x-amz-security-token:{}\n", token.expose_secret()));
            signed_headers.push_str(";x-amz-security-token");
        }

        let canonical_request = format!(
            "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
            req.method, req.canonical_uri, req.canonical_query
        );
        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

        let signing_key = self.signing_key(&date_stamp);
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.config.access_key_id, credential_scope, signed_headers, signature
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            HOST,
            HeaderValue::from_str(req.host)
                .map_err(|e| ObjectStoreError::Configuration(e.to_string()))?,
        );
        headers.insert(
            HeaderName::from_static("x-amz-content-sha256"),
            HeaderValue::from_str(&payload_hash)
                .map_err(|e| ObjectStoreError::Configuration(e.to_string()))?,
        );
        headers.insert(
            HeaderName::from_static("x-amz-date"),
            HeaderValue::from_str(&amz_date)
                .map_err(|e| ObjectStoreError::Configuration(e.to_string()))?,
        );
        if let Some(token) = &self.config.session_token {
            headers.insert(
                HeaderName::from_static("x-amz-security-token"),
                HeaderValue::from_str(token.expose_secret())
                    .map_err(|e| ObjectStoreError::Configuration(e.to_string()))?,
            );
        }
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&authorization)
                .map_err(|e| ObjectStoreError::Configuration(e.to_string()))?,
        );
        if let Some(ct) = req.content_type {
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(ct)
                    .map_err(|e| ObjectStoreError::Configuration(e.to_string()))?,
            );
        }

        Ok(headers)
    }

    /// Generates a deterministic SigV4 presigned URL using the supplied UTC timestamp.
    pub fn presign_at(
        &self,
        method: &'static str,
        key: &ObjectKey,
        expires_in_secs: u64,
        now: DateTime<Utc>,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        self.presign_at_with_content_type(method, key, expires_in_secs, None, now)
    }

    /// Generates a deterministic SigV4 presigned URL, optionally signing `content-type`.
    pub fn presign_at_with_content_type(
        &self,
        method: &'static str,
        key: &ObjectKey,
        expires_in_secs: u64,
        content_type: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        if !(1..=MAX_PRESIGN_EXPIRES_SECS).contains(&expires_in_secs) {
            return Err(ObjectStoreError::Configuration(format!(
                "S3 presigned URL expires_in_secs ({expires_in_secs}) must be in 1..={MAX_PRESIGN_EXPIRES_SECS}"
            )));
        }
        if let Some(ct) = content_type
            && ct.chars().any(|c| matches!(c, '\r' | '\n' | '\0'))
        {
            return Err(ObjectStoreError::Configuration(
                "S3 presigned URL content_type must not contain CR, LF, or NUL".to_string(),
            ));
        }
        let (host, canonical_uri, base_url) = self.host_and_path(Some(key.as_str()));
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", self.config.region);
        let credential = format!("{}/{}", self.config.access_key_id, credential_scope);

        let (canonical_headers, signed_headers) = match content_type {
            Some(ct) => (
                format!("content-type:{}\nhost:{host}\n", ct.trim()),
                "content-type;host",
            ),
            None => (format!("host:{host}\n"), "host"),
        };

        let mut query_params = vec![
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", percent_encode_component(&credential)),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", expires_in_secs.to_string()),
            (
                "X-Amz-SignedHeaders",
                percent_encode_component(signed_headers),
            ),
        ];
        if let Some(token) = &self.config.session_token {
            query_params.push((
                "X-Amz-Security-Token",
                percent_encode_component(token.expose_secret()),
            ));
        }
        query_params.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_query = query_params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
        );
        let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

        let signing_key = self.signing_key(&date_stamp);
        let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

        Ok(PresignedUrl {
            url: format!("{base_url}?{canonical_query}&X-Amz-Signature={signature}"),
            method,
            expires_in_secs,
        })
    }

    async fn send_signed(
        &self,
        method: Method,
        key: Option<&str>,
        canonical_query: &str,
        payload: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<reqwest::Response, ObjectStoreError> {
        let (host, canonical_uri, base_url) = self.host_and_path(key);
        let now = DateTime::<Utc>::from(SystemTime::now());
        let headers = self.build_signed_headers(&SigningRequest {
            method: method.as_str(),
            host: &host,
            canonical_uri: &canonical_uri,
            canonical_query,
            payload: &payload,
            content_type,
            now,
        })?;

        let url = if canonical_query.is_empty() {
            base_url
        } else {
            format!("{base_url}?{canonical_query}")
        };

        let mut req = self.client.request(method, url).headers(headers);
        if !payload.is_empty() {
            req = req.body(payload);
        }

        req.send()
            .await
            .map_err(|e| ObjectStoreError::Http(e.to_string()))
    }
}

impl ObjectStore for S3ObjectStore {
    fn put(
        &self,
        key: &ObjectKey,
        data: Vec<u8>,
        options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<ObjectMeta, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let size = data.len() as u64;
            let resp = self
                .send_signed(
                    Method::PUT,
                    Some(key.as_str()),
                    "",
                    data,
                    options.content_type.as_deref(),
                )
                .await?;

            if !resp.status().is_success() {
                return Err(map_status_error(resp.status(), key.as_str()));
            }

            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);

            Ok(ObjectMeta {
                key: key.into_inner(),
                size,
                content_type: options.content_type,
                etag,
                last_modified_epoch_secs: None,
            })
        })
    }

    fn get(&self, key: &ObjectKey) -> BoxFuture<'_, Result<Vec<u8>, ObjectStoreError>> {
        let key = key.clone();
        let max_bytes = self
            .config
            .max_get_bytes
            .unwrap_or(DEFAULT_S3_MAX_GET_BYTES);
        Box::pin(async move {
            let mut resp = self
                .send_signed(Method::GET, Some(key.as_str()), "", Vec::new(), None)
                .await?;

            if !resp.status().is_success() {
                return Err(map_status_error(resp.status(), key.as_str()));
            }

            if let Some(len) = resp.content_length()
                && len > max_bytes as u64
            {
                return Err(ObjectStoreError::Http(format!(
                    "S3 object '{}' size ({len} bytes) exceeds max_get_bytes ({max_bytes})",
                    key.as_str()
                )));
            }

            let mut out = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| ObjectStoreError::Http(e.to_string()))?
            {
                if out.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(ObjectStoreError::Http(format!(
                        "S3 object '{}' exceeded max_get_bytes ({max_bytes})",
                        key.as_str()
                    )));
                }
                out.extend_from_slice(&chunk);
            }
            Ok(out)
        })
    }

    fn head(&self, key: &ObjectKey) -> BoxFuture<'_, Result<ObjectMeta, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let resp = self
                .send_signed(Method::HEAD, Some(key.as_str()), "", Vec::new(), None)
                .await?;

            if !resp.status().is_success() {
                return Err(map_status_error(resp.status(), key.as_str()));
            }

            let headers = resp.headers();
            let size = headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let content_type = headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);
            let etag = headers
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);

            Ok(ObjectMeta {
                key: key.into_inner(),
                size,
                content_type,
                etag,
                last_modified_epoch_secs: None,
            })
        })
    }

    fn delete(&self, key: &ObjectKey) -> BoxFuture<'_, Result<(), ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let resp = self
                .send_signed(Method::DELETE, Some(key.as_str()), "", Vec::new(), None)
                .await?;

            if !resp.status().is_success() {
                return Err(map_status_error(resp.status(), key.as_str()));
            }
            Ok(())
        })
    }

    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectMeta>, ObjectStoreError>> {
        let prefix = prefix.to_string();
        let max_pages = self
            .config
            .max_list_pages
            .unwrap_or(DEFAULT_S3_MAX_LIST_PAGES)
            .max(1);
        Box::pin(async move {
            let mut all_items = Vec::new();
            let mut continuation_token: Option<String> = None;
            let mut page_count: usize = 0;

            loop {
                page_count += 1;
                if page_count > max_pages {
                    return Err(ObjectStoreError::Http(format!(
                        "S3 ListObjectsV2 pagination exceeded max_list_pages ({max_pages})"
                    )));
                }

                let mut params = Vec::with_capacity(3);
                if let Some(ref token) = continuation_token {
                    params.push(("continuation-token", percent_encode_component(token)));
                }
                params.push(("list-type", "2".to_string()));
                params.push(("prefix", percent_encode_component(&prefix)));
                params.sort_by(|a, b| a.0.cmp(b.0));

                let query = params
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("&");

                let resp = self
                    .send_signed(Method::GET, None, &query, Vec::new(), None)
                    .await?;

                if !resp.status().is_success() {
                    return Err(map_status_error(resp.status(), &prefix));
                }

                let xml = resp
                    .text()
                    .await
                    .map_err(|e| ObjectStoreError::Http(e.to_string()))?;
                let page = parse_list_objects_v2_page(&xml)?;
                all_items.extend(page.items);

                if page.is_truncated {
                    match page.next_continuation_token {
                        Some(next) if !next.is_empty() => {
                            if continuation_token.as_deref() == Some(next.as_str()) {
                                return Err(ObjectStoreError::Http(
                                    "S3 ListObjectsV2 returned repeating NextContinuationToken"
                                        .to_string(),
                                ));
                            }
                            continuation_token = Some(next);
                        }
                        _ => {
                            return Err(ObjectStoreError::Http(
                                "S3 ListObjectsV2 returned IsTruncated=true without NextContinuationToken"
                                    .to_string(),
                            ));
                        }
                    }
                } else {
                    break;
                }
            }

            Ok(all_items)
        })
    }

    fn presign_get(
        &self,
        key: &ObjectKey,
        expires_in_secs: u64,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let now = DateTime::<Utc>::from(SystemTime::now());
            self.presign_at("GET", &key, expires_in_secs, now)
        })
    }

    fn presign_put(
        &self,
        key: &ObjectKey,
        expires_in_secs: u64,
        options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let now = DateTime::<Utc>::from(SystemTime::now());
            self.presign_at_with_content_type(
                "PUT",
                &key,
                expires_in_secs,
                options.content_type.as_deref(),
                now,
            )
        })
    }
}

fn map_status_error(status: StatusCode, key: &str) -> ObjectStoreError {
    match status {
        StatusCode::NOT_FOUND => ObjectStoreError::NotFound(key.to_string()),
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
            ObjectStoreError::PermissionDenied(format!("S3 returned {status} for '{key}'"))
        }
        other => ObjectStoreError::Http(format!("S3 returned {other} for '{key}'")),
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn encode_s3_key(key: &str) -> String {
    key.split('/')
        .map(percent_encode_component)
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(&mut out, "%{byte:02X}");
        }
    }
    out
}

struct ListObjectsV2Page {
    items: Vec<ObjectMeta>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

fn parse_list_objects_v2_page(xml: &str) -> Result<ListObjectsV2Page, ObjectStoreError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut items = Vec::new();
    let mut is_truncated = false;
    let mut next_continuation_token: Option<String> = None;

    let mut in_contents = false;
    let mut current_tag: Option<String> = None;
    let mut current_text = String::new();

    let mut cur_key: Option<String> = None;
    let mut cur_size: u64 = 0;
    let mut cur_etag: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                let name_str: &str = name.as_ref();
                if name_str == "Contents" {
                    in_contents = true;
                    cur_key = None;
                    cur_size = 0;
                    cur_etag = None;
                } else {
                    current_tag = Some(name_str.to_string());
                    current_text.clear();
                }
            }
            Ok(Event::Text(e)) => {
                if current_tag.is_some() {
                    current_text.push_str(e.as_ref());
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if current_tag.is_some() {
                    if let Ok(Some(ch)) = e.resolve_char_ref() {
                        current_text.push(ch);
                    } else if let Some(resolved) = quick_xml::escape::resolve_xml_entity(e.as_ref())
                    {
                        current_text.push_str(resolved);
                    } else {
                        current_text.push('&');
                        current_text.push_str(e.as_ref());
                        current_text.push(';');
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if current_tag.is_some() {
                    current_text.push_str(e.as_ref());
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                let name_str: &str = name.as_ref();
                if name_str == "Contents" {
                    if let Some(key) = cur_key.take() {
                        items.push(ObjectMeta {
                            key,
                            size: cur_size,
                            content_type: None,
                            etag: cur_etag.take(),
                            last_modified_epoch_secs: None,
                        });
                    }
                    in_contents = false;
                } else if let Some(ref open) = current_tag
                    && open.as_str() == name_str
                {
                    if in_contents {
                        match open.as_str() {
                            "Key" => cur_key = Some(std::mem::take(&mut current_text)),
                            "Size" => {
                                cur_size = current_text.trim().parse::<u64>().unwrap_or(0);
                            }
                            "ETag" => cur_etag = Some(std::mem::take(&mut current_text)),
                            _ => {}
                        }
                    } else {
                        match open.as_str() {
                            "IsTruncated" => {
                                is_truncated = current_text.trim().eq_ignore_ascii_case("true");
                            }
                            "NextContinuationToken" => {
                                next_continuation_token = Some(std::mem::take(&mut current_text));
                            }
                            _ => {}
                        }
                    }
                    current_tag = None;
                    current_text.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ObjectStoreError::Http(format!(
                    "failed to parse S3 ListObjectsV2 XML: {e}"
                )));
            }
            _ => {}
        }
    }

    Ok(ListObjectsV2Page {
        items,
        is_truncated,
        next_continuation_token,
    })
}

#[cfg(test)]
fn parse_list_objects_v2_xml(xml: &str) -> Vec<ObjectMeta> {
    parse_list_objects_v2_page(xml)
        .expect("valid ListObjectsV2 XML")
        .items
}

#[cfg(test)]
#[path = "s3_tests.rs"]
mod tests;
