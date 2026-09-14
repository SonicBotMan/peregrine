//! `/events` WS bridge, tested end-to-end over a real Unix domain
//! socket: axum serve loop + WS upgrade + tungstenite client. The
//! events published on the bus while the socket is LIVE must arrive
//! as JSON frames in publish order (the bridge adds no filtering, no
//! mapping, no reordering — that's the point).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use peregrine_api::bus::EngineEvent;
use peregrine_api::task::{TaskId, TaskStatus};
use peregrine_scheduler::SchedulerConfig;
use peregrine_server::daemon::Daemon;
use tokio::net::UnixStream;
use tungstenite::client::IntoClientRequest;

/// Port fake that never runs (no task is added in this test — the
/// WS surface is exercised purely through direct bus publishes).
struct IdlePort;

impl peregrine_scheduler::DownloadPort for IdlePort {
    fn auto_download(
        &self,
        _job: peregrine_api::download::DownloadJob,
        _progress: peregrine_api::download::SharedProgressSink,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        peregrine_api::download::DownloadOutcome,
                        peregrine_api::error::ApiError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(std::future::pending())
    }

    fn purge(
        &self,
        _url: &str,
        _sink: &std::path::Path,
        _purge_files: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn events_stream_as_json_frames() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("t.sock");
    let daemon = Arc::new(
        Daemon::build_with_port(
            Some(&dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            Arc::new(IdlePort),
        )
        .unwrap(),
    );
    daemon.start().await.unwrap();
    let app = daemon.router();

    // Serve the router on a UDS; the socket identity for cleanup is
    // unnecessary here — tempdir drop removes everything.
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // WS handshake over the raw Unix stream (the URI is a handshake
    // formality — transport is whatever stream you hand over).
    let stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&sock))
        .await
        .expect("connect within 5s")
        .expect("connect");
    let req = "ws://localhost/events".into_client_request().unwrap();
    let (mut ws, _resp) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::client_async(req, stream),
    )
    .await
    .expect("handshake within 5s")
    .expect("handshake ok");

    // Connected: publish events on the bus, expect them as JSON in order.
    let e1 = EngineEvent::TaskAdded {
        id: TaskId::new("ws-1"),
        status: TaskStatus::Queued,
    };
    let e2 = EngineEvent::TaskProgress {
        id: TaskId::new("ws-1"),
        received: 100,
        total: Some(400),
    };
    daemon.bus.publish(e1.clone());
    daemon.bus.publish(e2.clone());

    let f1 = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("first frame within 5s")
        .expect("stream open")
        .expect("frame ok");
    let f2 = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("second frame within 5s")
        .expect("stream open")
        .expect("frame ok");

    let parse = |f| {
        let tungstenite::Message::Text(t) = f else {
            panic!("expected text frame, got {f:?}");
        };
        serde_json::from_str::<EngineEvent>(&t)
            .unwrap_or_else(|e| panic!("frame not a valid EngineEvent ({e}): {t}"))
    };
    assert_eq!(parse(f1), e1);
    assert_eq!(parse(f2), e2);

    // Client close terminates the bridge task cleanly (no hang, no
    // error — outbound future ends when inbound Close arrives).
    ws.close(None).await.expect("client close");
    tokio::time::sleep(Duration::from_millis(100)).await;
    daemon.sched.shutdown().await;
}
