//! Pure-Rust SigV4 S3-compatible object store backend.

use std::time::SystemTime;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, StatusCode};
use sha2::{Digest, Sha256};
use url::Url;

use crate::storage::{
    BoxFuture, ObjectKey, ObjectMeta, ObjectStore, ObjectStoreError, PresignedUrl, PutObjectOptions,
};

type HmacSha256 = Hmac<Sha256>;

/// Configuration for [`S3ObjectStore`].
#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub path_style: bool,
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
            || config.secret_access_key.is_empty()
        {
            return Err(ObjectStoreError::Configuration(
                "S3 bucket, region, access_key_id, and secret_access_key must be non-empty"
                    .to_string(),
            ));
        }

        let base_url = Url::parse(&config.endpoint).map_err(|e| {
            ObjectStoreError::Configuration(format!("invalid S3 endpoint URL: {e}"))
        })?;
        if base_url.host_str().is_none() {
            return Err(ObjectStoreError::Configuration(
                "S3 endpoint URL must include a host".to_string(),
            ));
        }

        let client = Client::builder().build().map_err(|e| {
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
        let k_secret = format!("AWS4{}", self.config.secret_access_key);
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
            canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
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
                HeaderValue::from_str(token)
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
        let (host, canonical_uri, base_url) = self.host_and_path(Some(key.as_str()));
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", self.config.region);
        let credential = format!("{}/{}", self.config.access_key_id, credential_scope);

        let mut query_params = vec![
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", percent_encode_component(&credential)),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", expires_in_secs.to_string()),
            ("X-Amz-SignedHeaders", "host".to_string()),
        ];
        if let Some(token) = &self.config.session_token {
            query_params.push(("X-Amz-Security-Token", percent_encode_component(token)));
        }
        query_params.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_query = query_params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_headers = format!("host:{host}\n");
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\nhost\nUNSIGNED-PAYLOAD"
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
        Box::pin(async move {
            let resp = self
                .send_signed(Method::GET, Some(key.as_str()), "", Vec::new(), None)
                .await?;

            if !resp.status().is_success() {
                return Err(map_status_error(resp.status(), key.as_str()));
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| ObjectStoreError::Http(e.to_string()))
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
        Box::pin(async move {
            let query = format!("list-type=2&prefix={}", percent_encode_component(&prefix));
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
            Ok(parse_list_objects_v2_xml(&xml))
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
        _options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let now = DateTime::<Utc>::from(SystemTime::now());
            self.presign_at("PUT", &key, expires_in_secs, now)
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

fn parse_list_objects_v2_xml(xml: &str) -> Vec<ObjectMeta> {
    let mut items = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Contents>") {
        let after_start = &rest[start + "<Contents>".len()..];
        let Some(end) = after_start.find("</Contents>") else {
            break;
        };
        let block = &after_start[..end];
        if let Some(key) = extract_xml_tag(block, "Key") {
            let size = extract_xml_tag(block, "Size")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let etag = extract_xml_tag(block, "ETag");
            items.push(ObjectMeta {
                key,
                size,
                content_type: None,
                etag,
                last_modified_epoch_secs: None,
            });
        }
        rest = &after_start[end + "</Contents>".len()..];
    }
    items
}

fn extract_xml_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(decode_xml_entities(&block[start..end]))
}

fn decode_xml_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ObjectKey, ObjectStore, PutObjectOptions};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn s3_sigv4_presigned_urls_match_canonical_format() {
        let store = S3ObjectStore::new(S3Config {
            endpoint: "https://s3.us-east-1.amazonaws.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: "my-bucket".to_string(),
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            path_style: false,
        })
        .expect("s3 config");

        let key = ObjectKey::new("folder/test.txt").expect("valid key");
        let fixed_time = chrono::DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let url1 = store
            .presign_at("GET", &key, 300, fixed_time)
            .expect("presign1");
        let url2 = store
            .presign_at("GET", &key, 300, fixed_time)
            .expect("presign2");

        assert_eq!(url1.url, url2.url);
        assert_eq!(url1.method, "GET");
        assert_eq!(url1.expires_in_secs, 300);
        assert!(
            url1.url
                .starts_with("https://my-bucket.s3.us-east-1.amazonaws.com/folder/test.txt?")
        );
        assert!(url1.url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(url1.url.contains(
            "X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20261126%2Fus-east-1%2Fs3%2Faws4_request"
        ));
        assert!(url1.url.contains("X-Amz-SignedHeaders=host"));
        assert!(url1.url.contains("X-Amz-Signature="));

        let put_url = store
            .presign_at("PUT", &key, 600, fixed_time)
            .expect("presign_put");
        assert_eq!(put_url.method, "PUT");
        assert_eq!(put_url.expires_in_secs, 600);
        assert!(put_url.url.contains("X-Amz-Signature="));

        let parsed = parse_list_objects_v2_xml(
            r#"<ListBucketResult><Contents><Key>a/b.txt</Key><Size>42</Size><ETag>&quot;e1&quot;</ETag></Contents></ListBucketResult>"#,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, "a/b.txt");
        assert_eq!(parsed[0].size, 42);
        assert_eq!(parsed[0].etag.as_deref(), Some("\"e1\""));
    }

    #[tokio::test]
    async fn s3_store_put_get_head_delete_against_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.expect("read");
                let req = String::from_utf8_lossy(&buf[..n]);
                assert!(
                    req.contains("authorization: AWS4-HMAC-SHA256")
                        || req.contains("Authorization: AWS4-HMAC-SHA256")
                );

                let response = if req.starts_with("PUT ") {
                    "HTTP/1.1 200 OK\r\nETag: \"abc123\"\r\nContent-Length: 0\r\n\r\n"
                } else if req.starts_with("GET ") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"
                } else if req.starts_with("HEAD ") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nETag: \"abc123\"\r\nContent-Type: text/plain\r\n\r\n"
                } else if req.starts_with("DELETE ") {
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
                } else {
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"
                };
                socket.write_all(response.as_bytes()).await.expect("write");
            }
        });

        let store = S3ObjectStore::new(S3Config {
            endpoint: format!("http://{addr}"),
            region: "us-east-1".to_string(),
            bucket: "test-bucket".to_string(),
            access_key_id: "test-key".to_string(),
            secret_access_key: "test-secret".to_string(),
            session_token: None,
            path_style: true,
        })
        .expect("s3 store");

        let key = ObjectKey::new("docs/hello.txt").expect("key");
        let meta = store
            .put(
                &key,
                b"hello".to_vec(),
                PutObjectOptions {
                    content_type: Some("text/plain".to_string()),
                },
            )
            .await
            .expect("put");
        assert_eq!(meta.etag.as_deref(), Some("\"abc123\""));

        let bytes = store.get(&key).await.expect("get");
        assert_eq!(bytes, b"hello");

        let head = store.head(&key).await.expect("head");
        assert_eq!(head.size, 5);
        assert_eq!(head.content_type.as_deref(), Some("text/plain"));

        store.delete(&key).await.expect("delete");
        server.await.expect("server");
    }
}
