//! Real-time WebSocket and SSE transport for Rume.

pub mod auth;
pub mod hub;
pub mod sse;
pub mod ws;

pub use auth::{RealtimeAuth, RealtimeTicket, RealtimeTicketSigner, redact_ticket_query};
pub use hub::{RealtimeConfig, RealtimeError, RealtimeEvent, RealtimeHub, RealtimeRecv};
pub use sse::sse_handler;
pub use ws::ws_handler;

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Extension;
    use axum::Router;
    use axum::routing::get;
    use rume_messaging::pubsub::InMemoryPubSub;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    const TEST_SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn build_test_router(hub: RealtimeHub, signer: RealtimeTicketSigner) -> Router {
        Router::new()
            .route("/ws/{room}", get(ws_handler))
            .route("/sse/{room}", get(sse_handler))
            .layer(Extension(hub))
            .layer(Extension(signer))
    }

    async fn spawn_test_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (addr, handle)
    }

    #[derive(Debug, PartialEq, Eq)]
    enum WsFrame {
        Text(String),
        Close,
        Ping(Vec<u8>),
        Pong(Vec<u8>),
    }

    struct TestWsClient {
        stream: BufReader<TcpStream>,
    }

    impl TestWsClient {
        async fn connect(addr: SocketAddr, path_and_query: &str) -> Result<Self, u16> {
            let mut stream = TcpStream::connect(addr).await.expect("connect tcp");
            // RFC 6455 §1.3 example key and its deterministic Sec-WebSocket-Accept.
            let key = "dGhlIHNhbXBsZSBub25jZQ==";
            let expected_accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
            let req = format!(
                "GET {path_and_query} HTTP/1.1\r\n\
                 Host: {addr}\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Key: {key}\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            );
            stream
                .write_all(req.as_bytes())
                .await
                .expect("write upgrade");

            let mut reader = BufReader::new(stream);
            let mut status_line = String::new();
            reader
                .read_line(&mut status_line)
                .await
                .expect("read status line");
            let code: u16 = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(500);

            let mut accept_ok = false;

            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.expect("read header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.trim().eq_ignore_ascii_case("sec-websocket-accept")
                    && v.trim() == expected_accept
                {
                    accept_ok = true;
                }
            }

            if code == 101 && accept_ok {
                Ok(Self { stream: reader })
            } else {
                Err(code)
            }
        }

        async fn read_frame(&mut self) -> Option<WsFrame> {
            let mut hdr = [0u8; 2];
            if self.stream.read_exact(&mut hdr).await.is_err() {
                return None;
            }
            let opcode = hdr[0] & 0x0f;
            let masked = (hdr[1] & 0x80) != 0;
            let len_code = hdr[1] & 0x7f;
            let len: usize = match len_code {
                126 => {
                    let mut b = [0u8; 2];
                    self.stream.read_exact(&mut b).await.ok()?;
                    u16::from_be_bytes(b) as usize
                }
                127 => {
                    let mut b = [0u8; 8];
                    self.stream.read_exact(&mut b).await.ok()?;
                    u64::from_be_bytes(b) as usize
                }
                n => n as usize,
            };
            let mask = if masked {
                let mut m = [0u8; 4];
                self.stream.read_exact(&mut m).await.ok()?;
                Some(m)
            } else {
                None
            };
            let mut payload = vec![0u8; len];
            if len > 0 {
                self.stream.read_exact(&mut payload).await.ok()?;
            }
            if let Some(m) = mask {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= m[i % 4];
                }
            }
            match opcode {
                0x1 => Some(WsFrame::Text(
                    String::from_utf8_lossy(&payload).into_owned(),
                )),
                0x8 => Some(WsFrame::Close),
                0x9 => Some(WsFrame::Ping(payload)),
                0xA => Some(WsFrame::Pong(payload)),
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn ws_and_sse_receive_tenant_scoped_broadcasts_and_isolate_tenants() {
        let hub = RealtimeHub::new(RealtimeConfig::default());
        let signer = RealtimeTicketSigner::new(TEST_SECRET).expect("signer");
        let ticket_a = signer
            .issue("user-a", "tenant-a", None, 60)
            .expect("ticket a");
        let ticket_b = signer
            .issue("user-b", "tenant-b", None, 60)
            .expect("ticket b");

        let (addr, server) = spawn_test_server(build_test_router(hub.clone(), signer)).await;

        let mut ws_a = TestWsClient::connect(addr, &format!("/ws/lobby?ticket={ticket_a}"))
            .await
            .expect("ws tenant-a connect");
        let mut ws_b = TestWsClient::connect(addr, &format!("/ws/lobby?ticket={ticket_b}"))
            .await
            .expect("ws tenant-b connect");

        let client = reqwest::Client::new();
        let mut sse_a = client
            .get(format!("http://{addr}/sse/lobby?ticket={ticket_a}"))
            .send()
            .await
            .expect("sse tenant-a");
        assert_eq!(sse_a.status(), reqwest::StatusCode::OK);

        // Give subscriptions a moment to register.
        tokio::time::sleep(Duration::from_millis(20)).await;

        hub.publish(
            "tenant-a",
            "lobby",
            RealtimeEvent::new("chat", serde_json::json!({"msg": "hello-a"})),
        )
        .await
        .expect("publish tenant-a");

        // WS tenant-a receives the message.
        let frame_a = tokio::time::timeout(Duration::from_secs(2), ws_a.read_frame())
            .await
            .expect("ws_a timeout")
            .expect("ws_a frame");
        match frame_a {
            WsFrame::Text(txt) => assert!(txt.contains("hello-a"), "unexpected ws_a text: {txt}"),
            other => panic!("expected Text frame for ws_a, got {other:?}"),
        }

        // SSE tenant-a receives the message.
        let chunk = tokio::time::timeout(Duration::from_secs(2), sse_a.chunk())
            .await
            .expect("sse_a timeout")
            .expect("sse_a chunk ok")
            .expect("sse_a chunk some");
        let sse_body = String::from_utf8_lossy(&chunk);
        assert!(
            sse_body.contains("hello-a"),
            "unexpected sse_a body: {sse_body}"
        );

        // WS tenant-b must NOT receive tenant-a's broadcast.
        let no_cross = tokio::time::timeout(Duration::from_millis(80), ws_b.read_frame()).await;
        assert!(
            no_cross.is_err(),
            "tenant-b must not receive tenant-a broadcast"
        );

        server.abort();
    }

    #[tokio::test]
    async fn realtime_hub_with_pubsub_deduplicates_origin_node_id() {
        let pubsub = Arc::new(InMemoryPubSub::new());
        let hub1 = RealtimeHub::new(RealtimeConfig::default()).with_pubsub(pubsub.clone());
        let hub2 = RealtimeHub::new(RealtimeConfig::default()).with_pubsub(pubsub);

        assert_ne!(hub1.node_id(), hub2.node_id());

        let mut sub1 = hub1.subscribe("acme", "updates").await.expect("sub1");
        let mut sub2 = hub2.subscribe("acme", "updates").await.expect("sub2");

        tokio::time::sleep(Duration::from_millis(20)).await;

        hub1.publish(
            "acme",
            "updates",
            RealtimeEvent::new("tick", serde_json::json!({"n": 1})),
        )
        .await
        .expect("publish");

        // hub1 receives exactly once (from local broadcast, NOT echoed back from pubsub).
        let ev1 = tokio::time::timeout(Duration::from_secs(1), sub1.recv())
            .await
            .expect("sub1 timeout");
        assert!(matches!(ev1, RealtimeRecv::Event(ref e) if e.event == "tick"));
        let dup1 = tokio::time::timeout(Duration::from_millis(80), sub1.recv()).await;
        assert!(
            dup1.is_err(),
            "hub1 must deduplicate its own origin_node_id"
        );

        // hub2 receives the event once via PubSub backplane.
        let ev2 = tokio::time::timeout(Duration::from_secs(1), sub2.recv())
            .await
            .expect("sub2 timeout");
        assert!(matches!(ev2, RealtimeRecv::Event(ref e) if e.event == "tick"));
    }

    #[tokio::test]
    async fn ws_heartbeat_timeout_closes_unresponsive_client() {
        let cfg = RealtimeConfig::default()
            .with_heartbeat_interval(Duration::from_millis(25))
            .with_heartbeat_timeout(Duration::from_millis(50));
        let hub = RealtimeHub::new(cfg);
        let signer = RealtimeTicketSigner::new(TEST_SECRET).expect("signer");
        let ticket = signer.issue("user-1", "acme", None, 60).expect("ticket");

        let (addr, server) = spawn_test_server(build_test_router(hub, signer)).await;
        let mut ws = TestWsClient::connect(addr, &format!("/ws/hb?ticket={ticket}"))
            .await
            .expect("ws connect");

        // Read frames without sending Pong; server should send Ping then Close (or EOF).
        let mut saw_close = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), ws.read_frame()).await {
                Ok(Some(WsFrame::Ping(_))) => {
                    // Intentionally do NOT reply with Pong.
                }
                Ok(Some(WsFrame::Close)) | Ok(None) => {
                    saw_close = true;
                    break;
                }
                Ok(Some(_)) => {}
                Err(_) => break,
            }
        }
        assert!(
            saw_close,
            "unresponsive client must be closed after heartbeat_timeout"
        );
        server.abort();
    }

    #[tokio::test]
    async fn realtime_graceful_shutdown_sends_ws_close_and_sse_reconnect() {
        let cfg = RealtimeConfig::default().with_shutdown_drain_timeout(Duration::from_secs(2));
        let hub = RealtimeHub::new(cfg);
        let signer = RealtimeTicketSigner::new(TEST_SECRET).expect("signer");
        let ticket = signer.issue("user-1", "acme", None, 60).expect("ticket");

        let (addr, server) = spawn_test_server(build_test_router(hub.clone(), signer)).await;
        let mut ws = TestWsClient::connect(addr, &format!("/ws/drain?ticket={ticket}"))
            .await
            .expect("ws connect");

        let client = reqwest::Client::new();
        let mut sse = client
            .get(format!("http://{addr}/sse/drain?ticket={ticket}"))
            .send()
            .await
            .expect("sse connect");
        assert_eq!(sse.status(), reqwest::StatusCode::OK);

        tokio::time::sleep(Duration::from_millis(20)).await;
        hub.shutdown().await;

        let frame = tokio::time::timeout(Duration::from_secs(2), ws.read_frame())
            .await
            .expect("ws close timeout");
        assert_eq!(
            frame,
            Some(WsFrame::Close),
            "WS must receive Close frame on shutdown"
        );

        let mut sse_text = String::new();
        while let Ok(Ok(Some(bytes))) =
            tokio::time::timeout(Duration::from_secs(1), sse.chunk()).await
        {
            sse_text.push_str(&String::from_utf8_lossy(&bytes));
            if sse_text.contains("reconnect") {
                break;
            }
        }
        assert!(
            sse_text.contains("event: reconnect"),
            "SSE must emit event: reconnect on shutdown, got: {sse_text}"
        );

        server.abort();
    }

    #[tokio::test]
    async fn realtime_auth_rejects_missing_or_cross_tenant_ticket() {
        let hub = RealtimeHub::new(RealtimeConfig::default());
        let signer = RealtimeTicketSigner::new(TEST_SECRET).expect("signer");
        let tenant_a_ticket = signer
            .issue("user-1", "tenant-a", None, 60)
            .expect("ticket");
        let room_a_ticket = signer
            .issue("user-1", "tenant-a", Some("room-a".to_string()), 60)
            .expect("room ticket");

        let (addr, server) = spawn_test_server(build_test_router(hub, signer)).await;
        let client = reqwest::Client::new();

        // Missing ticket -> 401 Unauthorized.
        let missing = client
            .get(format!("http://{addr}/sse/room-a"))
            .send()
            .await
            .expect("missing ticket req");
        assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);

        // Cross-tenant query parameter mismatch -> 403 Forbidden.
        let cross_tenant = client
            .get(format!(
                "http://{addr}/sse/room-a?ticket={tenant_a_ticket}&tenant_id=tenant-b"
            ))
            .send()
            .await
            .expect("cross tenant req");
        assert_eq!(cross_tenant.status(), reqwest::StatusCode::FORBIDDEN);

        // Room-scoped ticket targeting another room -> 403 Forbidden.
        let wrong_room = client
            .get(format!("http://{addr}/sse/room-b?ticket={room_a_ticket}"))
            .send()
            .await
            .expect("wrong room req");
        assert_eq!(wrong_room.status(), reqwest::StatusCode::FORBIDDEN);

        // Redaction helper masks ticket query parameter.
        let redacted = redact_ticket_query(&format!("/ws/room-a?ticket={tenant_a_ticket}&foo=bar"));
        assert!(!redacted.contains(&tenant_a_ticket));
        assert!(redacted.contains("ticket=[REDACTED]"));

        server.abort();
    }

    #[tokio::test]
    async fn realtime_hub_rejects_colon_identifiers_and_surfaces_lagged() {
        let hub = RealtimeHub::new(RealtimeConfig::default().with_channel_capacity(2));

        let bad_tenant = hub
            .publish(
                "bad:tenant",
                "room",
                RealtimeEvent::new("ev", serde_json::json!(1)),
            )
            .await;
        assert!(matches!(
            bad_tenant,
            Err(RealtimeError::InvalidIdentifier {
                kind: "tenant_id",
                ..
            })
        ));

        let bad_room = hub.subscribe("acme", "bad:room").await;
        assert!(matches!(
            bad_room,
            Err(RealtimeError::InvalidIdentifier { kind: "room", .. })
        ));

        let signer = RealtimeTicketSigner::new(TEST_SECRET).expect("signer");
        assert!(matches!(
            signer.issue("user-1", "acme\nbad", None, 60),
            Err(RealtimeError::InvalidIdentifier {
                kind: "tenant_id",
                ..
            })
        ));
        assert!(matches!(
            signer.issue("user-1", "acme", Some("room\x00bad".to_string()), 60),
            Err(RealtimeError::InvalidIdentifier { kind: "room", .. })
        ));
        assert!(matches!(
            signer.issue("user-1", "", None, 60),
            Err(RealtimeError::InvalidIdentifier {
                kind: "tenant_id",
                ..
            })
        ));
        assert!(matches!(
            signer.issue("user-1", "acme", Some("a:b".to_string()), 60),
            Err(RealtimeError::InvalidIdentifier { kind: "room", .. })
        ));

        let mut sub = hub.subscribe("acme", "fast").await.expect("subscribe");
        for i in 0..5 {
            hub.publish(
                "acme",
                "fast",
                RealtimeEvent::new("n", serde_json::json!(i)),
            )
            .await
            .expect("publish");
        }
        assert!(matches!(sub.recv().await, RealtimeRecv::Lagged(_)));
    }

    #[tokio::test]
    async fn realtime_auth_accepts_auth_user_and_session_from_extensions() {
        use axum::extract::FromRequestParts;
        use rume_security::auth::{AuthUser, Session};

        let mut req_user = axum::http::Request::builder()
            .uri("/ws/room")
            .body(())
            .unwrap();
        req_user
            .extensions_mut()
            .insert(AuthUser::new("u1").with_tenant_id("t1"));
        let (mut parts, _) = req_user.into_parts();
        let auth = RealtimeAuth::from_request_parts(&mut parts, &())
            .await
            .expect("AuthUser with tenant_id should authenticate");
        assert_eq!(auth.subject, "u1");
        assert_eq!(auth.tenant_id, "t1");

        let mut req_session = axum::http::Request::builder()
            .uri("/sse/room")
            .body(())
            .unwrap();
        req_session
            .extensions_mut()
            .insert(Session::new("s1", "u2", 60).with_tenant_id("t2"));
        let (mut parts_s, _) = req_session.into_parts();
        let auth_s = RealtimeAuth::from_request_parts(&mut parts_s, &())
            .await
            .expect("Session with tenant_id should authenticate");
        assert_eq!(auth_s.subject, "u2");
        assert_eq!(auth_s.tenant_id, "t2");
    }
}
