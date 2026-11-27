//! Axum Server-Sent Events (SSE) handler with lag handling and graceful reconnect drain.

use std::convert::Infallible;

use axum::Extension;
use axum::extract::Path;
use axum::response::Sse;
use axum::response::sse::{Event, KeepAlive};
use futures_util::stream::Stream;

use crate::realtime::auth::RealtimeAuth;
use crate::realtime::hub::{RealtimeError, RealtimeHub, RealtimeRecv};

/// Axum handler streaming tenant-scoped room events over Server-Sent Events.
pub async fn sse_handler(
    auth: RealtimeAuth,
    Path(room): Path<String>,
    Extension(hub): Extension<RealtimeHub>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RealtimeError> {
    auth.authorize_room(&room)?;
    let sub = hub.subscribe(&auth.tenant_id, &room).await?;
    let guard = hub.track_connection();
    let keepalive_interval = hub.config().heartbeat_interval;
    let tenant_id = auth.tenant_id;

    let stream = futures_util::stream::unfold(
        (sub, guard, tenant_id, room, false),
        |(mut sub, guard, tenant_id, room, done)| async move {
            if done {
                return None;
            }
            match sub.recv().await {
                RealtimeRecv::Event(ev) => {
                    let mut sse_ev = Event::default().event(ev.event).data(ev.data.to_string());
                    if let Some(id) = ev.id {
                        sse_ev = sse_ev.id(id);
                    }
                    Some((Ok(sse_ev), (sub, guard, tenant_id, room, false)))
                }
                RealtimeRecv::Lagged(skipped) => {
                    tracing::warn!(
                        skipped,
                        tenant_id = %tenant_id,
                        room = %room,
                        "sse subscriber lagged"
                    );
                    let sse_ev = Event::default()
                        .event("lagged")
                        .data(format!(r#"{{"skipped":{skipped}}}"#));
                    Some((Ok(sse_ev), (sub, guard, tenant_id, room, false)))
                }
                RealtimeRecv::Shutdown => {
                    let sse_ev = Event::default()
                        .event("reconnect")
                        .data("server shutting down");
                    Some((Ok(sse_ev), (sub, guard, tenant_id, room, true)))
                }
                RealtimeRecv::Closed => None,
            }
        },
    );

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(keepalive_interval)))
}
