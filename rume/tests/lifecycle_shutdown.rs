use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
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
    let ctx: AppContext<(), (), ()> = AppContext::builder((), ()).build();

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

    let router = Router::new().route("/slow", get(slow_handler)).route(
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
    let builder = Rume::builder(ctx).router(router).on_shutdown(move |_| {
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

    // Wait for server to start and declare Ready
    for _ in 0..100 {
        if server_ctx.lifecycle.state() == LifecycleState::Ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
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

    // Wait for signal to transition lifecycle & availability to Terminating
    for _ in 0..100 {
        if server_ctx.lifecycle.state() == LifecycleState::Terminating {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

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

#[tokio::test]
async fn shutdown_hooks_abort_when_exceeding_aggregate_timeout() {
    let ctx: AppContext<(), (), ()> = AppContext::builder((), ()).build();
    let server_ctx = ctx.clone();

    let hook1_finished = Arc::new(AtomicBool::new(false));
    let hook2_finished = Arc::new(AtomicBool::new(false));

    let h1_flag = hook1_finished.clone();
    let h2_flag = hook2_finished.clone();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let builder = Rume::builder(ctx)
        .router(Router::new())
        .shutdown_timeout(Duration::from_millis(80))
        .on_shutdown(move |_| {
            let flag = h1_flag.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                flag.store(true, Ordering::SeqCst);
            }
        })
        .on_shutdown(move |_| {
            let flag = h2_flag.clone();
            async move {
                // This hook takes 250ms, exceeding the aggregate timeout
                tokio::time::sleep(Duration::from_millis(250)).await;
                flag.store(true, Ordering::SeqCst);
            }
        });

    let server_task = tokio::spawn(async move {
        builder
            .run_with_listener_and_signal(listener, async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    for _ in 0..100 {
        if server_ctx.lifecycle.state() == LifecycleState::Ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let start = std::time::Instant::now();
    shutdown_tx.send(()).unwrap();

    server_task.await.unwrap();
    let elapsed = start.elapsed();

    // Must terminate around the 80ms timeout, well below the combined 280ms hooks duration
    assert!(
        elapsed < Duration::from_millis(180),
        "shutdown took too long: {:?}",
        elapsed
    );
    assert!(hook1_finished.load(Ordering::SeqCst));
    assert!(
        !hook2_finished.load(Ordering::SeqCst),
        "second slow hook should have been aborted by aggregate timeout"
    );
    assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Terminated);
}

#[tokio::test]
async fn startup_hook_failure_executes_cleanup_hooks_and_fails_run() {
    let ctx: AppContext<(), (), ()> = AppContext::builder((), ()).build();
    let server_ctx = ctx.clone();

    let cleanup_ran = Arc::new(AtomicBool::new(false));
    let cleanup_flag = cleanup_ran.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let builder = Rume::builder(ctx)
        .router(Router::new())
        .on_ready(|_| async move { Err::<(), _>("database connection refused") })
        .on_shutdown(move |_| {
            let flag = cleanup_flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
            }
        });

    let result = builder
        .run_with_listener_and_signal(listener, std::future::pending::<()>())
        .await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("database connection refused"));
    assert!(cleanup_ran.load(Ordering::SeqCst));
    assert_eq!(server_ctx.lifecycle.state(), LifecycleState::Terminated);
}
