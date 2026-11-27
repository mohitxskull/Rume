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
        ..Default::default()
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
        ..Default::default()
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

#[tokio::test]
async fn s3_presign_put_includes_content_type_in_signed_headers() {
    let store = S3ObjectStore::new(S3Config {
        endpoint: "https://s3.us-east-1.amazonaws.com".to_string(),
        region: "us-east-1".to_string(),
        bucket: "my-bucket".to_string(),
        access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        session_token: None,
        path_style: false,
        ..Default::default()
    })
    .expect("s3 config");

    let key = ObjectKey::new("folder/data.json").expect("valid key");
    let with_ct = store
        .presign_put(
            &key,
            300,
            PutObjectOptions {
                content_type: Some("application/json".to_string()),
            },
        )
        .await
        .expect("presign_put with content_type");
    assert!(
        with_ct
            .url
            .contains("X-Amz-SignedHeaders=content-type%3Bhost"),
        "presign_put with content_type must sign content-type;host, got: {}",
        with_ct.url
    );

    let without_ct = store
        .presign_put(&key, 300, PutObjectOptions::default())
        .await
        .expect("presign_put without content_type");
    assert!(
        without_ct.url.contains("X-Amz-SignedHeaders=host&")
            || without_ct.url.ends_with("X-Amz-SignedHeaders=host"),
        "presign_put without content_type must sign only host, got: {}",
        without_ct.url
    );
}

#[tokio::test]
async fn s3_list_paginates_truncated_responses_with_continuation_token() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let page1 = r#"<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>tok+1/2</NextContinuationToken><Contents><Key>logs/a.txt</Key><Size>10</Size><ETag>&quot;e1&quot;</ETag></Contents></ListBucketResult>"#;
        let page2 = r#"<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>logs/b.txt</Key><Size>20</Size><ETag>&quot;e2&quot;</ETag></Contents></ListBucketResult>"#;

        for page_idx in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.expect("read");
            let req = String::from_utf8_lossy(&buf[..n]);
            let body = if page_idx == 0 {
                assert!(
                    req.starts_with("GET /test-bucket?list-type=2&prefix=logs%2F "),
                    "unexpected first list request: {req}"
                );
                page1
            } else {
                assert!(
                    req.starts_with(
                        "GET /test-bucket?continuation-token=tok%2B1%2F2&list-type=2&prefix=logs%2F "
                    ),
                    "unexpected second list request: {req}"
                );
                page2
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
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
        ..Default::default()
    })
    .expect("s3 store");

    let items = store.list("logs/").await.expect("paginated list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].key, "logs/a.txt");
    assert_eq!(items[1].key, "logs/b.txt");
    server.await.expect("server");
}

#[test]
fn s3_xml_parser_decodes_entities_once_without_double_decoding() {
    let xml = r#"<ListBucketResult><Contents><Key>a&amp;lt;b</Key><Size>1</Size><ETag>&quot;a&amp;amp;b&quot;</ETag></Contents><Contents><Key>&lt;tag attr=&quot;1&quot; o=&apos;2&apos;&gt;&amp;</Key><Size>2</Size></Contents></ListBucketResult>"#;
    let items = parse_list_objects_v2_xml(xml);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].key, "a&lt;b");
    assert_eq!(items[0].etag.as_deref(), Some("\"a&amp;b\""));
    assert_eq!(items[1].key, "<tag attr=\"1\" o='2'>&");
}

#[test]
fn s3_config_debug_redacts_secret_access_key_and_session_token() {
    let cfg = S3Config {
        endpoint: "https://s3.us-east-1.amazonaws.com".to_string(),
        region: "us-east-1".to_string(),
        bucket: "my-bucket".to_string(),
        access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
        secret_access_key: "super_secret_aws_key".to_string(),
        session_token: Some("super_secret_session_token".to_string()),
        path_style: false,
        ..Default::default()
    };
    let dbg = format!("{cfg:?}");
    assert!(dbg.contains("[REDACTED]"));
    assert!(!dbg.contains("super_secret_aws_key"));
    assert!(!dbg.contains("super_secret_session_token"));
}

#[test]
fn s3_rejects_malformed_xml_invalid_bucket_and_invalid_presign_params() {
    // 1. Malformed XML returns Err instead of silent truncation
    let xml_err =
        parse_list_objects_v2_page("<ListBucketResult><Contents><Key>a</Wrong></ListBucketResult>")
            .err()
            .expect("malformed XML must return Err");
    assert!(matches!(xml_err, ObjectStoreError::Http(_)));

    // 2. Invalid bucket names rejected
    for bad_bucket in [
        "bad/bucket",
        "bad\\bucket",
        "bad?q",
        "bad#f",
        "bad..b",
        "bad bucket",
    ] {
        let err = S3ObjectStore::new(S3Config {
            endpoint: "https://s3.us-east-1.amazonaws.com".to_string(),
            region: "us-east-1".to_string(),
            bucket: bad_bucket.to_string(),
            access_key_id: "ak".to_string(),
            secret_access_key: "sk".to_string(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            matches!(err, ObjectStoreError::Configuration(_)),
            "expected Configuration error for bucket {bad_bucket:?}"
        );
    }

    // 3. Invalid presign TTL (0, 604_801) and CRLF content_type rejected
    let store = S3ObjectStore::new(S3Config {
        endpoint: "https://s3.us-east-1.amazonaws.com".to_string(),
        region: "us-east-1".to_string(),
        bucket: "my-bucket".to_string(),
        access_key_id: "ak".to_string(),
        secret_access_key: "sk".to_string(),
        ..Default::default()
    })
    .unwrap();
    let key = ObjectKey::new("a.txt").unwrap();
    let now = chrono::Utc::now();

    assert!(matches!(
        store.presign_at("GET", &key, 0, now),
        Err(ObjectStoreError::Configuration(_))
    ));
    assert!(matches!(
        store.presign_at("GET", &key, 604_801, now),
        Err(ObjectStoreError::Configuration(_))
    ));
    assert!(matches!(
        store.presign_at_with_content_type("PUT", &key, 60, Some("text/plain\r\nX-Evil: 1"), now),
        Err(ObjectStoreError::Configuration(_))
    ));
}

#[tokio::test]
async fn s3_list_rejects_repeating_continuation_token() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let stuck_page = r#"<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>stuck-tok</NextContinuationToken><Contents><Key>a.txt</Key><Size>1</Size></Contents></ListBucketResult>"#;
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await.expect("read");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n{stuck_page}",
                stuck_page.len()
            );
            socket.write_all(response.as_bytes()).await.expect("write");
        }
    });

    let store = S3ObjectStore::new(S3Config {
        endpoint: format!("http://{addr}"),
        region: "us-east-1".to_string(),
        bucket: "test-bucket".to_string(),
        access_key_id: "ak".to_string(),
        secret_access_key: "sk".to_string(),
        path_style: true,
        ..Default::default()
    })
    .unwrap();

    let err = store
        .list("")
        .await
        .expect_err("repeating continuation token must fail");
    assert!(matches!(err, ObjectStoreError::Http(ref msg) if msg.contains("repeating")));
    server.await.expect("server");
}
