//! `/events` — the WS bridge (M2-d, PROPOSAL §3.2: "WS 事件流").
//!
//! One direction that matters: `EventBus` → client. Every state
//! change the daemon knows arrives as a typed `EngineEvent` already
//! JSON-encoded by serde (bus is the wire format — no server-side
//! DTO mapping to rot).
//!
//! The client→server direction is deliberately empty. Control goes
//! through REST; a WS that also accepts commands is two protocols to
//! keep consistent for zero benefit. The only inbound handling is
//! Close (client hung up) and Ping (axum auto-answers before we see
//! it).
//!
//! Lossiness is a contract, not an accident: a slow client's
//! broadcast slot overflows and we log-and-continue (the UI
//! refetches via REST on gap suspicion; progress is monotone, the
//! next event repaints the truth). A stuck TCP peer that stops
//! draining cannot backpressure the daemon — sends get a deadline
//! and the socket is dropped on breach.

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use std::sync::Arc;
use std::time::Duration;

use crate::api::AppState;
use crate::daemon::Daemon;

/// How long one event send may block before the peer is declared
/// stuck and disconnected. Generous (seconds, not ms): a burst of
/// events after a gap legitimately queues several frames.
const SEND_DEADLINE: Duration = Duration::from_secs(10);

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| stream(socket, state.0))
}

async fn stream(mut socket: WebSocket, daemon: Arc<Daemon>) {
    let mut rx = daemon.bus.subscribe();
    loop {
        tokio::select! {
            // Outbound: bus events win the select so a chatty client
            // can't starve event delivery by... not talking (the
            // other branch only fires on close).
            biased;
            recv = rx.recv() => match recv {
                Ok(event) => {
                    let frame = match serde_json::to_string(&event) {
                        Ok(json) => json,
                        Err(e) => {
                            // Bus events are plain data; a serde
                            // failure here is a code bug, not input.
                            tracing::error!(error = %e, "event encode failed; closing ws");
                            break;
                        }
                    };
                    let sent = tokio::time::timeout(
                        SEND_DEADLINE,
                        socket.send(Message::Text(frame.into())),
                    )
                    .await;
                    match sent {
                        Ok(Ok(_)) => {}
                        Ok(Err(_)) | Err(_) => break, // peer gone or stuck
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Slow consumer: n events skipped. Progress is
                    // monotone — the next event carries fresh truth —
                    // but the client must KNOW it missed deltas, or a
                    // GUI frozen during the burst keeps rendering a
                    // stale "running" forever (R2 P1-4). Emit one
                    // synthetic resync frame; clients refetch
                    // GET /tasks on receipt. Keeping the connection
                    // (vs disconnect-reconnect) is deliberate:
                    // reconnect storms fire exactly when the bus is
                    // busiest, which is when lag happens.
                    tracing::debug!(skipped = n, "ws subscriber lagged; emitting resync");
                    let frame = format!(
                        "{{\"type\":\"resync_required\",\"skipped\":{n}}}"
                    );
                    let sent = tokio::time::timeout(
                        SEND_DEADLINE,
                        socket.send(Message::Text(frame.into())),
                    )
                    .await;
                    match sent {
                        Ok(Ok(_)) => {}
                        Ok(Err(_)) | Err(_) => break,
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            // Inbound: close means done; anything else (text/binary/
            // ping) is ignored by contract — control is REST-only.
            msg = socket.recv() => match msg {
                None | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
        }
    }
    // axum sends Close on drop; nothing else to clean (bus receivers
    // are refcounted — dropping ours is the unsubscribe).
}
