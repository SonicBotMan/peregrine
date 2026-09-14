//! Event bridge (M5): daemon WS `/events` → MCP
//! `notifications/resources/updated`.
//!
//! One bridge task per session, started lazily on the first
//! `subscribe`. It reconnects forever with capped backoff — a
//! daemon that is down at subscribe time (or restarts) must not
//! kill notifications for the session's lifetime.
//!
//! Event → URI mapping is intentionally coarse: every
//! [`EngineEvent`] touching a task invalidates both `task://{id}`
//! and the list resource `tasks://`. Settings changes don't flow on
//! the bus (REST writes only); `settings://` has no push source —
//! documented in the server instructions.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use peregrine_api::bus::EngineEvent;
use rmcp::model::{
    ResourceUpdatedNotification, ResourceUpdatedNotificationParam, ServerNotification,
};
use tungstenite::Message;

use crate::Inner;

pub const RES_TASKS_URI: &str = "tasks://";

const RECONNECT_MIN: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(10);

/// Start the bridge exactly once per session. Idempotent.
pub fn ensure_bridge(inner: Arc<Inner>, events_url: &str) {
    if inner.bridge.get().is_some() {
        return;
    }
    let task = tokio::spawn(bridge_loop(inner.clone(), events_url.to_string()));
    let _ = inner.bridge.set(task);
}

async fn bridge_loop(inner: Arc<Inner>, events_url: String) {
    let mut backoff = RECONNECT_MIN;
    loop {
        match tokio_tungstenite::connect_async(&events_url).await {
            Ok((ws, _resp)) => {
                backoff = RECONNECT_MIN;
                tracing::debug!(url = %events_url, "event bridge connected");
                if let Err(e) = bridge_session(&inner, ws).await {
                    tracing::debug!(error = %e, "event bridge session ended");
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, url = %events_url, "event bridge connect failed");
            }
        }
        // Session ended (daemon restart / network drop): retry with
        // capped exponential backoff. Cancel-never: the task is
        // reaped at process exit — subscriptions can outlive any
        // single WS connection.
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// One WS connection: read frames, map to URIs, push notifications
/// for subscribed URIs through the session peer.
async fn bridge_session(
    inner: &Arc<Inner>,
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> anyhow::Result<()> {
    let (_sink, mut stream) = ws.split();

    while let Some(frame) = stream.next().await {
        let text = match frame? {
            Message::Text(t) => t,
            Message::Close(_) => anyhow::bail!("daemon closed the event stream"),
            // Pings are answered by the tungstenite codec; binaries
            // never carry events on this bus.
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {
                continue;
            }
        };
        let event: EngineEvent = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                // Daemon and this crate share the EngineEvent type;
                // a decode failure means version skew — log and keep
                // the bridge alive (the next event still parses or
                // doesn't; one bad frame must not kill the session).
                tracing::warn!(error = %e, "event decode failed");
                continue;
            }
        };
        let uris = event_uris(&event);
        notify(inner, &uris).await;
    }
    anyhow::bail!("event stream ended")
}

/// Every event touching a task invalidates the task resource and
/// the list resource. (Progress events are high-frequency; the
/// notify path filters by subscription, so cost stays bounded by
/// what the client asked for.)
fn event_uris(e: &EngineEvent) -> Vec<String> {
    let id = match e {
        EngineEvent::TaskAdded { id, .. }
        | EngineEvent::TaskStarted { id }
        | EngineEvent::TaskProgress { id, .. }
        | EngineEvent::TaskStatusChanged { id, .. }
        | EngineEvent::TaskCompleted { id }
        | EngineEvent::TaskFailed { id, .. }
        | EngineEvent::TaskRemoved { id, .. }
        | EngineEvent::TaskLimitChanged { id, .. } => id,
    };
    vec![format!("task://{id}"), RES_TASKS_URI.to_string()]
}

async fn notify(inner: &Arc<Inner>, uris: &[String]) {
    let Some(peer) = inner.peer.get() else {
        // Peer not seen yet (no request since session start — only
        // possible if subscribe raced the very first exchange).
        return;
    };
    let hits: Vec<String> = {
        let subs = inner.subs.lock().await;
        uris.iter().filter(|u| subs.contains(*u)).cloned().collect()
    };
    for uri in hits {
        let notification = ServerNotification::ResourceUpdatedNotification(
            ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(uri.clone())),
        );
        if let Err(e) = peer.send_notification(notification).await {
            // A dead peer ends the session anyway; log and move on.
            tracing::debug!(error = %e, %uri, "resource update push failed");
        }
    }
}
