//! CLI↔daemon end-to-end over a REAL Unix socket: axum serve loop +
//! `DaemonClient` (hyper over UDS) + scripted engine port. Pins the
//! full client contract: typed round-trips, error propagation (the
//! daemon's 409 body surfaces verbatim), and the scheduler facade
//! actually reaching the engine.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use peregrine_api::AddTaskRequest;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_api::error::ApiError;
use peregrine_api::task::{Priority, Task, TaskStatus};
use peregrine_cli::DaemonClient;
use peregrine_scheduler::{DownloadPort, SchedulerConfig};
use peregrine_server::daemon::Daemon;
use tokio_util::sync::CancellationToken;

/// Hangs until cancelled (the row status IS the assertion here).
struct HangingPort;

impl DownloadPort for HangingPort {
    fn auto_download(
        &self,
        _job: DownloadJob,
        _progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        Box::pin(async move {
            cancel.cancelled().await;
            Err(ApiError::Cancelled)
        })
    }

    fn purge(
        &self,
        _url: &str,
        _sink: &std::path::Path,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

struct Rig {
    dir: tempfile::TempDir,
    daemon: Arc<Daemon>,
    client: DaemonClient,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Arc::new(
        Daemon::build_with_port(
            Some(&dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            Arc::new(HangingPort),
        )
        .unwrap(),
    );
    daemon.start().await.unwrap();

    let sock = dir.path().join("cli.sock");
    let app = daemon.router();
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Rig {
        client: DaemonClient::new(sock),
        daemon,
        dir,
    }
}

#[tokio::test]
async fn add_list_pause_resume_remove_roundtrip() {
    let rig = rig().await;
    let save = rig.dir.path().join("a.bin");

    // add → typed Task back, priority honored.
    let task: Task = rig
        .client
        .request_json(
            "POST",
            "/tasks",
            Some(&AddTaskRequest {
                url: "http://example.test/a.bin".to_string(),
                save_path: save.display().to_string(),
                priority: Priority::High,
            }),
        )
        .await
        .unwrap();
    assert_eq!(task.priority, Priority::High);
    assert_eq!(task.status, TaskStatus::Queued);

    // list sees it.
    let all: Vec<Task> = rig
        .client
        .request_json("GET", "/tasks", None::<&serde_json::Value>)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);

    // duplicate add → the daemon's 409 body, surfaced verbatim.
    let err = rig
        .client
        .request_json::<_, Task>(
            "POST",
            "/tasks",
            Some(&AddTaskRequest {
                url: "http://example.test/a.bin".to_string(),
                save_path: save.display().to_string(),
                priority: Priority::Normal,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("409"),
        "expected 409 in error, got: {err}"
    );

    // pause → row flips (scheduler facade: cancel token fires, worker
    // no-ops, state stays paused).
    let id = task.id.clone();
    let paused: Task = rig
        .client
        .request_json(
            "POST",
            &format!("/tasks/{id}/pause"),
            None::<&serde_json::Value>,
        )
        .await
        .unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);

    // resume re-queues.
    let resumed: Task = rig
        .client
        .request_json(
            "POST",
            &format!("/tasks/{id}/resume"),
            None::<&serde_json::Value>,
        )
        .await
        .unwrap();
    assert_eq!(resumed.status, TaskStatus::Queued);

    // remove → {"removed": true} + row gone.
    let removed: serde_json::Value = rig
        .client
        .request_json(
            "DELETE",
            &format!("/tasks/{id}"),
            None::<&serde_json::Value>,
        )
        .await
        .unwrap();
    assert_eq!(removed["removed"], serde_json::json!(true));
    let after: Vec<Task> = rig
        .client
        .request_json("GET", "/tasks", None::<&serde_json::Value>)
        .await
        .unwrap();
    assert!(after.is_empty());

    let _ = tokio::time::timeout(Duration::from_secs(5), rig.daemon.sched.shutdown()).await;
}

#[tokio::test]
async fn ping_health_over_uds() {
    let rig = rig().await;
    let health = rig.client.ping().await.unwrap();
    assert_eq!(health.name, "peregrine");
    let _ = tokio::time::timeout(Duration::from_secs(5), rig.daemon.sched.shutdown()).await;
}
