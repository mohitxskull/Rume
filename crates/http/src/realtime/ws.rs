//! Axum WebSocket upgrade handler with ping/pong heartbeats and graceful drain.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{Path, WebSocketUpgrade};
use axum::response::Response;

use crate::realtime::auth::RealtimeAuth;
use crate::realtime::hub::{RealtimeError, RealtimeHub, RealtimeRecv, RealtimeSubscription};

/// Axum handler upgrading a request to a tenant-scoped WebSocket room stream.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    auth: RealtimeAuth,
    Path(room): Path<String>,
    Extension(hub): Extension<RealtimeHub>,
) -> Result<Response, RealtimeError> {
    auth.authorize_room(&room)?;
    let sub = hub.subscribe(&auth.tenant_id, &room).await?;
    let max_size = hub.config().max_message_size;

    Ok(ws
        .max_message_size(max_size)
        .on_upgrade(move |socket| handle_ws_socket(socket, hub, sub, auth, room)))
}

async fn send_with_timeout(
    socket: &mut WebSocket,
    msg: Message,
    timeout: std::time::Duration,
) -> bool {
    // Why: A client with a zero TCP receive window would otherwise stall `socket.send().await`
    // indefinitely, preventing heartbeat enforcement and `RealtimeRecv::Shutdown` from running.
    matches!(
        tokio::time::timeout(timeout, socket.send(msg)).await,
        Ok(Ok(()))
    )
}

async fn handle_ws_socket(
    mut socket: WebSocket,
    hub: RealtimeHub,
    mut sub: RealtimeSubscription,
    auth: RealtimeAuth,
    room: String,
) {
    let _conn_guard = hub.track_connection();
    let hb_interval = hub.config().heartbeat_interval;
    let hb_timeout = hub.config().heartbeat_timeout;

    let mut ping_ticker = tokio::time::interval(hb_interval);
    ping_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick so we wait one full interval before the first Ping.
    ping_ticker.tick().await;

    let mut awaiting_pong_since: Option<tokio::time::Instant> = None;

    loop {
        let pong_deadline = awaiting_pong_since.map(|t| t + hb_timeout);

        tokio::select! {
            biased;
            recv = sub.recv() => match recv {
                RealtimeRecv::Event(ev) => {
                    match serde_json::to_string(&ev) {
                        Ok(json) => {
                            if !send_with_timeout(&mut socket, Message::Text(json.into()), hb_timeout).await {
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "failed to serialize realtime ws event");
                        }
                    }
                }
                RealtimeRecv::Lagged(skipped) => {
                    tracing::warn!(
                        skipped,
                        tenant_id = %auth.tenant_id,
                        room = %room,
                        "websocket subscriber lagged"
                    );
                    let lag_json = serde_json::json!({
                        "event": "lagged",
                        "data": { "skipped": skipped }
                    })
                    .to_string();
                    if !send_with_timeout(&mut socket, Message::Text(lag_json.into()), hb_timeout).await {
                        break;
                    }
                }
                RealtimeRecv::Shutdown => {
                    let _ = send_with_timeout(
                        &mut socket,
                        Message::Close(Some(CloseFrame {
                            code: 1001,
                            reason: "server shutting down".into(),
                        })),
                        hb_timeout,
                    )
                    .await;
                    break;
                }
                RealtimeRecv::Closed => break,
            },
            _ = async {
                match pong_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                tracing::warn!(
                    tenant_id = %auth.tenant_id,
                    room = %room,
                    "websocket heartbeat timeout; closing unresponsive client"
                );
                let _ = send_with_timeout(
                    &mut socket,
                    Message::Close(Some(CloseFrame {
                        code: 1001,
                        reason: "heartbeat timeout".into(),
                    })),
                    hb_timeout,
                )
                .await;
                break;
            },
            _ = ping_ticker.tick() => {
                if awaiting_pong_since.is_none() {
                    if !send_with_timeout(&mut socket, Message::Ping(Bytes::new()), hb_timeout).await {
                        break;
                    }
                    awaiting_pong_since = Some(tokio::time::Instant::now());
                }
            },
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Pong(_))) => {
                    awaiting_pong_since = None;
                }
                Some(Ok(Message::Ping(payload))) => {
                    if !send_with_timeout(&mut socket, Message::Pong(payload), hb_timeout).await {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    }
}
