use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use rume::app::state::{AvailabilityState, LifecycleState};
use rume::app::{AppContext, Rume};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

async fn send_get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).await.unwrap();
    let status = resp
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, resp)
}

#[tokio::test]
async fn graceful_shutdown_transitions_to_refusing_traffic_while_request_in_flight() {
    let ctx = AppContext::builder((), ()).build();

    let (slow_started_tx, slow_started_rx) = oneshot::channel::<()>();
    let (slow_finish_tx, slow_finish_rx) = oneshot::channel::<()>();

    let slow_started_tx = Arc::new(tokio::sync::Mutex::new(Some(slow_started_tx)));
    let slow_finish_rx = Arc::new(tokio::sync::Mutex::new(Some(slow_finish_rx)));

    let slow_handler = {
        let slow_started_tx = slow_started_tx.clone();
        let slow_finish_rx = slow_finish_rx.clone();
        move || {
            let slow_started_tx = slow_started_tx.clone();
            let slow_finish_rx = slow_finish_rx.clone();
            async move {
                if let Some(tx) = slow_started_tx.lock().await.take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = slow_finish_rx.lock().await.take() {
                    let _ = rx.await;
                }
                "slow completed"
            }
        }
    };

    let router = Router::new()
        .route("/slow", get(slow_handler))
        .route(
            "/health/ready",
            get(|State(app_ctx): State<AppContext<(), (), ()>>| async move {
                let avail = app_ctx.availability.state();
                if avail == AvailabilityState::Ready {
                    (axum::http::StatusCode::OK, "ready").into_response()
                } else {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "refusing_traffic",
                    )
                        .into_response()
                }
            }),
        );

    let shutdown_hook_executed = Arc::new(AtomicBool::new(false));
    let hook_flag = shutdown_hook_executed.clone();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_ctx = ctx.clone();
    let builder = Rume::builder(ctx)
        .router(router)
        .on_shutdown(move |_| {
            let hook_flag = hook_flag.clone();
            async move {
                hook_flag.store(true, Ordering::SeqCst);
            }
        });

    let shutdown_fut = async move {
        let _ = shutdown_rx.await;
    };

    let server_task = tokio::spawn(async move {
        builder
            .run_with_listener_and_signal(listener, shutdown_fut)
            .await
            .unwrap();
    });

    // Give server a moment to start and declare Ready
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Ready);
    assert_eq!(server_ctx.availability.state(), AvailabilityState::Ready);

    // Initial health check is OK (200)
    let (status, body) = send_get(addr, "/health/ready").await;
    assert_eq!(status, 200);
    assert!(body.contains("ready"));

    // Connect and dispatch the slow request in the background
    let slow_task = tokio::spawn(async move { send_get(addr, "/slow").await });

    // Wait until the slow handler is actively in flight
    slow_started_rx.await.unwrap();

    // Trigger graceful shutdown
    shutdown_tx.send(()).unwrap();

    // Give the signal a moment to transition lifecycle & availability
    tokio::time::sleep(Duration::from_millis(50)).await;

    // WHILE the slow request is still held open:
    // 1. Availability must be RefusingTraffic
    // 2. Lifecycle must be Terminating
    assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Terminating);
    assert_eq!(
        server_ctx.availability.state(),
        AvailabilityState::RefusingTraffic
    );

    // Now release the in-flight slow request
    slow_finish_tx.send(()).unwrap();

    // The slow request completes successfully with 200 OK
    let (slow_status, slow_body) = slow_task.await.unwrap();
    assert_eq!(slow_status, 200);
    assert!(slow_body.contains("slow completed"));

    // The server finishes shutdown
    server_task.await.unwrap();

    // Shutdown hook must have executed
    assert!(shutdown_hook_executed.load(Ordering::SeqCst));

    // Final state must be Terminated
    assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Terminated);
    assert_eq!(server_ctx.availability.state(), AvailabilityState::Unknown);
}
