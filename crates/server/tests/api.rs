//! M2-d REST surface tests: the full daemon stack (scheduler loop +
//! task manager + bus) behind the axum router, driven over HTTP
//! `oneshot` with a scripted engine port — no network, no HTTP
//! engine, but every routing/state/CAS rule live.
//!
//! What these tests pin down:
//! - CRUD happy paths return the domain `Task` verbatim (wire =
//!   domain types, no DTO drift).
//! - Error truth: 404 `not_found`, 409 `illegal_transition` /
//!   `duplicate_active`, 422 `invalid_input` — body shape
//!   `{error, message}` always.
//! - Writes flow through the scheduler facade: a paused Running
//!   task actually reaches the engine's cancel token (pause drives
//!   policy, not just a row update).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use http_body_util::BodyExt;
use peregrine_api::AddTaskRequest;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_api::error::ApiError;
use peregrine_api::task::{Priority, TaskStatus};
use peregrine_scheduler::{DownloadPort, SchedulerConfig};
use peregrine_server::daemon::Daemon;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Engine fake: never finishes until cancelled. Counters let tests
/// assert the port was actually entered/left.
struct HangingPort {
    entered: AtomicUsize,
    cancelled: AtomicUsize,
    /// Live limit pokes received (url, bps) — M3-b.
    limits: std::sync::Mutex<Vec<(String, Option<u64>)>>,
}

impl DownloadPort for HangingPort {
    fn auto_download(
        &self,
        _job: DownloadJob,
        _progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let cancelled = &self.cancelled;
        Box::pin(async move {
            cancel.cancelled().await;
            cancelled.fetch_add(1, Ordering::SeqCst);
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

    fn set_task_limit(&self, url: &str, _sink: &std::path::Path, bps: Option<u64>) {
        self.limits.lock().unwrap().push((url.to_string(), bps));
    }
}

struct Rig {
    app: axum::Router,
    daemon: Arc<Daemon>,
    port: Arc<HangingPort>,
    dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(HangingPort {
        entered: AtomicUsize::new(0),
        cancelled: AtomicUsize::new(0),
        limits: std::sync::Mutex::new(Vec::new()),
    });
    let daemon = Arc::new(
        Daemon::build_with_port(
            Some(&dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            port.clone() as Arc<dyn DownloadPort>,
        )
        .unwrap(),
    );
    daemon.start().await.unwrap();
    let app = daemon.router();
    Rig {
        app,
        daemon,
        port,
        dir,
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Best-effort: a leaked loop keeps the tempdir alive until
        // process end — tests create a Rig each, so force a drain.
        // Detached on purpose: Drop is sync, the test is done, and a
        // leaked shutdown attempt is harmless (the store is a temp
        // file; SQLite data lands via WAL checkpoint at last close).
        let sched = self.daemon.sched.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(5), sched.shutdown()).await;
        });
    }
}

async fn json_req(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut req = axum::http::Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

async fn wait_for(cond: impl Fn() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition never became true");
}

#[tokio::test]
async fn crud_roundtrip_returns_domain_tasks() {
    let rig = rig().await;
    let save = rig.dir.path().join("file.bin");

    // CREATE → 201 + the row itself.
    let (status, created) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!(AddTaskRequest {
            url: "http://example.test/f.bin".to_string(),
            save_path: save.display().to_string(),
            priority: Priority::High,
        })),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["priority"], "high");
    assert_eq!(created["status"], "queued");
    let id = created["id"].as_str().unwrap().to_string();

    // Scheduler picks it up (write path is the facade — wake included).
    wait_for(|| rig.port.entered.load(Ordering::SeqCst) == 1).await;

    // GET list + GET one.
    let (status, list) = json_req(&rig.app, "GET", "/tasks", None).await;
    assert_eq!(status, 200);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, one) = json_req(&rig.app, "GET", &format!("/tasks/{id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(one["id"].as_str().unwrap(), id);

    // Status filter round-trips the query param.
    let (status, queued) = json_req(&rig.app, "GET", "/tasks?status=queued", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        queued.as_array().unwrap().len(),
        0,
        "task is running, not queued"
    );

    // DELETE through the facade (cancel + row drop).
    let (status, body) = json_req(&rig.app, "DELETE", &format!("/tasks/{id}"), None).await;
    assert_eq!(status, 200, "{body}");
    wait_for(|| rig.port.cancelled.load(Ordering::SeqCst) == 1).await;
    let (status, gone) = json_req(&rig.app, "GET", &format!("/tasks/{id}"), None).await;
    assert_eq!(status, 404);
    assert_eq!(gone["error"], "not_found");
}

#[tokio::test]
async fn invalid_inputs_map_to_422_with_codes() {
    let rig = rig().await;
    let save = rig.dir.path().join("x");

    let (status, body) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "",
            "save_path": save.display().to_string(),
        })),
    )
    .await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"], "invalid_input");
    assert!(body["message"].as_str().unwrap().contains("url"));
}

#[tokio::test]
async fn duplicate_active_maps_to_409() {
    let rig = rig().await;
    let save = rig.dir.path().join("dup.bin");
    let req = serde_json::json!({
        "url": "http://example.test/dup.bin",
        "save_path": save.display().to_string(),
    });
    let (status, _) = json_req(&rig.app, "POST", "/tasks", Some(req.clone())).await;
    assert_eq!(status, 201);
    let (status, body) = json_req(&rig.app, "POST", "/tasks", Some(req)).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], "duplicate_active");
}

#[tokio::test]
async fn pause_running_task_reaches_engine_and_row() {
    let rig = rig().await;
    let save = rig.dir.path().join("p.bin");
    let (status, created) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "http://example.test/p.bin",
            "save_path": save.display().to_string(),
        })),
    )
    .await;
    assert_eq!(status, 201);
    let id = created["id"].as_str().unwrap().to_string();
    wait_for(|| rig.port.entered.load(Ordering::SeqCst) == 1).await;

    let (status, paused) = json_req(&rig.app, "POST", &format!("/tasks/{id}/pause"), None).await;
    assert_eq!(status, 200, "{paused}");
    assert_eq!(paused["status"], "paused");

    // The engine saw the cancellation (pause drove POLICY, not just
    // the row — this is the facade-vs-direct-write distinction).
    wait_for(|| rig.port.cancelled.load(Ordering::SeqCst) == 1).await;

    // Pausing again is a state-machine truth, not a server bug.
    let (status, again) = json_req(&rig.app, "POST", &format!("/tasks/{id}/pause"), None).await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["error"], "illegal_transition");
    let _ = TaskStatus::Queued; // import sanity for status assertions
}

// ---- M3-b: rate-limit + settings surface -------------------------------

#[tokio::test]
async fn task_limit_endpoint_persists_and_pokes_live() {
    let rig = rig().await;
    let save = rig.dir.path().join("lim.bin");

    let (status, created) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "http://example.test/lim.bin",
            "save_path": save.display().to_string(),
        })),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    wait_for(|| rig.port.entered.load(Ordering::SeqCst) == 1).await;

    // PUT /tasks/{id}/limit → 200 with the updated row.
    let (status, body) = json_req(
        &rig.app,
        "PUT",
        &format!("/tasks/{id}/limit"),
        Some(serde_json::json!({ "bps": 131072 })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["speed_limit_bps"], 131072);

    // The RUNNING engine was poked live through the port.
    {
        let limits = rig.port.limits.lock().unwrap();
        assert_eq!(limits.len(), 1, "{limits:?}");
        assert_eq!(limits[0].1, Some(131072));
    }

    // Row persisted (GET reflects it).
    let (status, row) = json_req(&rig.app, "GET", &format!("/tasks/{id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(row["speed_limit_bps"], 131072);

    // Unknown id → 404 not_found, no poke.
    let (status, body) = json_req(
        &rig.app,
        "PUT",
        "/tasks/nope/limit",
        Some(serde_json::json!({ "bps": 1 })),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"], "not_found");
    assert_eq!(rig.port.limits.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn settings_global_limit_roundtrip_and_persistence() {
    let rig = rig().await;

    // Fresh daemon: unlimited.
    let (status, s) = json_req(&rig.app, "GET", "/settings", None).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["global_limit_bps"], 0);

    // Set → applies live + echoes.
    let (status, s) = json_req(
        &rig.app,
        "PUT",
        "/settings",
        Some(serde_json::json!({ "global_limit_bps": 262144 })),
    )
    .await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["global_limit_bps"], 262144);
    assert_eq!(rig.daemon.global_budget.bps(), 262144);

    // Persistence: a NEW daemon over the same db restores at start().
    let daemon2 = Arc::new(
        Daemon::build_with_port(
            Some(&rig.dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            {
                let p: Arc<dyn DownloadPort> = Arc::new(HangingPort {
                    entered: AtomicUsize::new(0),
                    cancelled: AtomicUsize::new(0),
                    limits: std::sync::Mutex::new(Vec::new()),
                });
                p
            },
        )
        .unwrap(),
    );
    daemon2.start().await.unwrap();
    assert_eq!(
        daemon2.global_budget.bps(),
        262144,
        "persisted global limit must survive a daemon restart"
    );
}

// --- M3-c1: segment telemetry endpoint --------------------------------

#[tokio::test]
async fn segments_of_unknown_task_is_404() {
    let rig = rig().await;
    let (status, body) = json_req(&rig.app, "GET", "/tasks/nope-0000/segments", None).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn segments_of_single_stream_task_is_empty_list() {
    // A task the fake port never segmented: the engine tables hold
    // no plan rows for it → `[]`, not 404 and not null (the GUI's
    // per-connection panel renders "single stream" off the empty
    // list; null would be a third state to handle).
    let rig = rig().await;
    let (status, task) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "https://example.com/f.bin",
            "save_path": "/tmp/seg-empty.bin",
        })),
    )
    .await;
    assert_eq!(status, 201, "{task}");
    let id = task["id"].as_str().unwrap().to_string();

    let (status, body) = json_req(&rig.app, "GET", &format!("/tasks/{id}/segments"), None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, serde_json::json!([]), "no plan rows → empty list");
}

#[tokio::test]
async fn segments_view_reflects_stored_plan_rows() {
    // Seed the ENGINE table directly (url, sink → plan + cursors):
    // the daemon's segments_of must resolve the WIRE id through the
    // task row into the engine row and map every cursor with derived
    // numbers. 100 bytes, [0,49]+[50,99]; the second half done.
    let rig = rig().await;
    let (status, task) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "https://example.com/seg.bin",
            "save_path": "/tmp/seg-plan.bin",
        })),
    )
    .await;
    assert_eq!(status, 201, "{task}");
    let id = task["id"].as_str().unwrap().to_string();

    use peregrine_storage::Store;
    let store = Store::open(&rig.dir.path().join("tasks.db")).unwrap();
    let row = store
        .upsert_task(
            "https://example.com/seg.bin",
            std::path::Path::new("/tmp/seg-plan.bin"),
            Some(100),
            None,
        )
        .await
        .unwrap();
    store
        .replace_segments(row, &[(0, 49), (50, 99)])
        .await
        .unwrap();
    store.update_cursor(row, 1, 50).await.unwrap();

    let (status, body) = json_req(&rig.app, "GET", &format!("/tasks/{id}/segments"), None).await;
    assert_eq!(status, 200, "{body}");
    let segs = body.as_array().unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0]["idx"], 0);
    assert_eq!(segs[0]["start"], 0);
    assert_eq!(segs[0]["end"], 49);
    assert_eq!(segs[0]["done"], 0);
    assert_eq!(segs[0]["len"], 50);
    assert_eq!(segs[0]["pct"], 0.0);
    assert_eq!(segs[1]["idx"], 1);
    assert_eq!(segs[1]["done"], 50);
    assert_eq!(segs[1]["frontier"], 100);
    assert_eq!(segs[1]["pct"], 1.0);
}

#[tokio::test]
async fn segments_pct_is_fractional_in_flight() {
    // In-flight (the panel's primary state): done < len must yield
    // a true float fraction, never integer division and never > 1.
    // 4-byte segment, 1 byte confirmed → pct 0.25.
    let rig = rig().await;
    let (status, task) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "https://example.com/frac.bin",
            "save_path": "/tmp/frac.bin",
        })),
    )
    .await;
    assert_eq!(status, 201, "{task}");
    let id = task["id"].as_str().unwrap().to_string();

    use peregrine_storage::Store;
    let store = Store::open(&rig.dir.path().join("tasks.db")).unwrap();
    let row = store
        .upsert_task(
            "https://example.com/frac.bin",
            std::path::Path::new("/tmp/frac.bin"),
            Some(4),
            None,
        )
        .await
        .unwrap();
    store.replace_segments(row, &[(0, 3)]).await.unwrap();
    store.update_cursor(row, 0, 1).await.unwrap();

    let (status, body) = json_req(&rig.app, "GET", &format!("/tasks/{id}/segments"), None).await;
    assert_eq!(status, 200, "{body}");
    let seg = &body.as_array().unwrap()[0];
    assert_eq!(seg["done"], 1);
    assert_eq!(seg["len"], 4);
    assert_eq!(seg["frontier"], 1);
    let pct = seg["pct"].as_f64().unwrap();
    assert!((pct - 0.25).abs() < f64::EPSILON, "pct={pct}");
}

#[tokio::test]
async fn segments_alias_same_url_sink_shares_engine_row() {
    // Two wire tasks aliasing one (url, sink) resolve to the SAME
    // engine row — current contract: the store keys on (url, sink),
    // so telemetry is shared, not per-wire-id. This test LOCKS that
    // (no silent leak onto a different task's numbers); changing
    // the aliasing rule later must update this pin.
    let rig = rig().await;
    let (status, task) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": "https://example.com/alias.bin",
            "save_path": "/tmp/alias.bin",
        })),
    )
    .await;
    assert_eq!(status, 201, "{task}");
    let id1 = task["id"].as_str().unwrap().to_string();
    // The duplicate guard blocks a second add while the first row is
    // non-terminal, but terminal-then-re-add IS legal — the alias
    // window is real. Bypassing the API guard here (direct row
    // insert) because this test pins the TELEMETRY contract, not
    // the guard (add_duplicate_active_is_409 already covers that).
    use peregrine_api::task::{Task, TaskId};
    use peregrine_storage::Store;
    let store = Store::open(&rig.dir.path().join("tasks.db")).unwrap();
    let id2 = TaskId::new(format!("{id1}-alias"));
    let mut t2 = Task::new(id2.clone(), "https://example.com/alias.bin", "/tmp/alias.bin");
    t2.status = TaskStatus::Failed;
    store.insert_download(&t2).await.unwrap();

    let row = store
        .upsert_task(
            "https://example.com/alias.bin",
            std::path::Path::new("/tmp/alias.bin"),
            Some(10),
            None,
        )
        .await
        .unwrap();
    store.replace_segments(row, &[(0, 9)]).await.unwrap();
    store.update_cursor(row, 0, 3).await.unwrap();

    for id in [&id1, id2.as_str()] {
        let (status, body) =
            json_req(&rig.app, "GET", &format!("/tasks/{id}/segments"), None).await;
        assert_eq!(status, 200, "{body}");
        let segs = body.as_array().unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["done"], 3);
        assert_eq!(segs[0]["len"], 10);
    }
}
