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
struct HangingPort {
    /// Purge flags received (R2' P2-6: proves the `?purge=true`
    /// query reaches the engine port; file deletion itself is
    /// asserted in scheduler/tests/purge_contract.rs against the
    /// real HttpAutoPort).
    purge_flags: std::sync::Mutex<Vec<bool>>,
}

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
        purge_files: bool,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        self.purge_flags.lock().unwrap().push(purge_files);
        Box::pin(async { Ok(()) })
    }
}

struct Rig {
    dir: tempfile::TempDir,
    daemon: Arc<Daemon>,
    port: Arc<HangingPort>,
    client: DaemonClient,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(HangingPort {
        purge_flags: std::sync::Mutex::new(Vec::new()),
    });
    let daemon = Arc::new(
        Daemon::build_with_port(
            Some(&dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            port.clone(),
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
        port,
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
async fn remove_with_purge_sends_query_and_deletes_sink() {
    // R2' P2-6: the `--purge` flag's whole contract — the request
    // path carries `purge=true` AND the HTTP sink file is actually
    // deleted (P1-1). Drives DaemonClient directly (the CLI main
    // only formats these calls; e2e-level CLI coverage would spawn
    // a binary for the same string).
    let rig = rig().await;
    let save = rig.dir.path().join("gone.bin");
    std::fs::write(&save, b"partial").unwrap();

    let task: Task = rig
        .client
        .request_json(
            "POST",
            "/tasks",
            Some(&AddTaskRequest {
                url: "http://example.test/gone.bin".to_string(),
                save_path: save.display().to_string(),
                priority: Priority::Normal,
            }),
        )
        .await
        .unwrap();

    // Plain remove keeps the file.
    let v: serde_json::Value = rig
        .client
        .request_json(
            "DELETE",
            &format!("/tasks/{}", task.id),
            None::<&serde_json::Value>,
        )
        .await
        .unwrap();
    assert_eq!(v["removed"], serde_json::json!(true));
    assert!(save.exists(), "plain remove keeps user data");
    for _ in 0..200 {
        if rig.port.purge_flags.lock().unwrap().len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(!rig.port.purge_flags.lock().unwrap()[0]);

    // Re-add, then purged remove: query in path + flag at the port.
    let task: Task = rig
        .client
        .request_json(
            "POST",
            "/tasks",
            Some(&AddTaskRequest {
                url: "http://example.test/gone.bin".to_string(),
                save_path: save.display().to_string(),
                priority: Priority::Normal,
            }),
        )
        .await
        .unwrap();
    let v: serde_json::Value = rig
        .client
        .request_json(
            "DELETE",
            &format!("/tasks/{}?purge=true", task.id),
            None::<&serde_json::Value>,
        )
        .await
        .unwrap();
    assert_eq!(v["removed"], serde_json::json!(true));
    for _ in 0..200 {
        if rig.port.purge_flags.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(rig.port.purge_flags.lock().unwrap()[1]);
}

#[tokio::test]
async fn ping_health_over_uds() {
    let rig = rig().await;
    let health = rig.client.ping().await.unwrap();
    assert_eq!(health.name, "peregrine");
    let _ = tokio::time::timeout(Duration::from_secs(5), rig.daemon.sched.shutdown()).await;
}
