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

/// Per-push bound. Generous for a healthy client draining a burst
/// of progress events; short enough that a wedged peer only costs
/// one bridge tick before the event is dropped (R2' P1-2).
const PUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Start the bridge exactly once per session. Idempotent.
pub fn ensure_bridge(inner: Arc<Inner>, events_url: &str) {
    if inner.bridge.get().is_some() {
        return;
    }
    let task = tokio::spawn(bridge_loop(inner.clone(), events_url.to_string()));
    let _ = inner.bridge.set(task);
}

async fn bridge_loop(inner: Arc<Inner>, events_url: String) {
    // Handshake request built ONCE, before the loop: `--auth-token`
    // daemons demand the bearer header on the WS upgrade itself,
    // and `connect_async(&str)` would synthesize a bare GET without
    // it. `Request<()>` is `Clone`, so every reconnect reuses the
    // same stamped request. A malformed URL (operator typo in
    // `--events`) disables the bridge with one ERROR line instead
    // of retrying garbage forever.
    use tungstenite::client::IntoClientRequest;
    let handshake = events_url.as_str().into_client_request().and_then(|mut r| {
        if let Some(t) = &inner.token {
            r.headers_mut().insert(
                "authorization",
                format!("Bearer {t}").parse().map_err(|_| {
                    tungstenite::Error::Url(tungstenite::error::UrlError::UnableToConnect(
                        "token is not a valid header value".into(),
                    ))
                })?,
            );
        }
        Ok(r)
    });
    let handshake = match handshake {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(
                url = %events_url,
                error = %e,
                "event bridge: unusable events url — bridge disabled"
            );
            return;
        }
    };
    let mut backoff = RECONNECT_MIN;
    let mut consecutive_failures = 0u32;
    loop {
        match tokio_tungstenite::connect_async(handshake.clone()).await {
            Ok((ws, _resp)) => {
                backoff = RECONNECT_MIN;
                consecutive_failures = 0;
                tracing::debug!(url = %events_url, "event bridge connected");
                if let Err(e) = bridge_session(&inner, ws).await {
                    // A notify timeout / dead peer is a SESSION
                    // signal (M5.1 P1-2): stop the bridge entirely —
                    // a session whose peer is gone has no work left.
                    if e.downcast_ref::<BridgeDead>().is_some() {
                        tracing::debug!("event bridge: session gone, stopping");
                        return;
                    }
                    tracing::debug!(error = %e, "event bridge session ended");
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                // M5.1 P1-1: a daemon that never comes up must not
                // spam debug forever — surface it once at ERROR (the
                // operator-facing signal), then drop back to debug.
                if consecutive_failures == 3 {
                    tracing::error!(
                        url = %events_url,
                        error = %e,
                        "event bridge: daemon unreachable after 3 attempts \
                         (is `peregrined --tcp` running?) — retrying quietly"
                    );
                } else {
                    tracing::debug!(error = %e, url = %events_url, "event bridge connect failed");
                }
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
/// Marker: the session peer is gone/wedged — the bridge should
/// stop (per-session task leak fix, M5.1 P1-2).
#[derive(Debug, thiserror::Error)]
#[error("session peer is gone")]
struct BridgeDead;

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
        if notify(inner, &uris).await.is_err() {
            anyhow::bail!(BridgeDead);
        }
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
        | EngineEvent::TaskLimitChanged { id, .. }
        // queue re-rank: same task uri fan-out, no extra shape
        | EngineEvent::TaskPriorityChanged { id, .. } => id,
    };
    vec![format!("task://{id}"), RES_TASKS_URI.to_string()]
}

async fn notify(inner: &Arc<Inner>, uris: &[String]) -> Result<(), BridgeDead> {
    let Some(peer) = inner.peer.get() else {
        // Peer not seen yet (no request since session start — only
        // possible if subscribe raced the very first exchange).
        return Ok(());
    };
    let hits: Vec<String> = {
        let subs = inner.subs.lock().await;
        uris.iter().filter(|u| subs.contains(*u)).cloned().collect()
    };
    for uri in hits {
        let notification = ServerNotification::ResourceUpdatedNotification(
            ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(uri.clone())),
        );
        // M5.1 P1-2: bounded send — a wedged peer must not park the
        // bridge on an unbounded await. R2' P1-2: a TIMEOUT is
        // backpressure (an LLM host mid-generation not draining
        // stdio), not death — drop THIS event's remaining pushes and
        // keep bridging; only a hard send error (dead session)
        // kills the bridge.
        let sent = tokio::time::timeout(PUSH_TIMEOUT, peer.send_notification(notification)).await;
        match sent {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                // Send failed: the session peer is dead — stop the
                // bridge (M5.1 P1-2: leak fix; a dead session's
                // bridge has no reader left).
                tracing::debug!(error = %e, %uri, "resource update push failed");
                return Err(BridgeDead);
            }
            Err(_) => {
                tracing::debug!(%uri, "resource update push timed out (backpressure)");
                return Ok(());
            }
        }
    }
    Ok(())
}
