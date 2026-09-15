//! P2-4 (contract-fix R2 follow-up): the re-add instant-complete
//! chain at FULL daemon depth — real `Daemon::build` (real
//! `HttpAutoPort` + real SQLite store + real scheduler loop), real
//! axum file server, driven over HTTP `oneshot`. The api.rs rig
//! mocks the port, which by construction can never produce store
//! rows; this file pins the production chain instead:
//!
//! 1. first add → segmented download completes; the completed
//!    segment table is served (the #31 contract: rows survive
//!    completion);
//! 2. re-add of the same (url, sink) → Route 1 short-circuits onto
//!    the done rows and completes with ZERO new server fetches
//!    (not even a probe) and `received == total` (the #30 fix).
//!
//! If this test goes red after touching the resume path, the
//! regression is in the production chain, not in a mock.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http_body_util::BodyExt;
use peregrine_engine_http::SegmentConfig;
use peregrine_scheduler::SchedulerConfig;
use peregrine_server::daemon::Daemon;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Ranged file server (same semantics as engine-http's
/// tests/auto.rs helper): 206 + Content-Range for a real Range,
/// 200 full body otherwise, ETag on every answer. HEAD rides the
/// GET handler (axum auto-strips the body).
async fn serve_ranged(req_headers: &HeaderMap, full: &[u8], etag: &str) -> Response {
    let Some(range) = req_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(s, e)| {
            let start = s.parse::<u64>().ok()?;
            let end = if e.is_empty() {
                full.len() as u64 - 1
            } else {
                e.parse::<u64>().ok()?
            };
            Some((start, end))
        })
    else {
        let mut resp = (StatusCode::OK, full.to_vec()).into_response();
        resp.headers_mut()
            .insert(header::ETAG, header::HeaderValue::from_str(etag).unwrap());
        return resp;
    };
    let (start, end) = range;
    if start >= full.len() as u64 || end >= full.len() as u64 || end < start {
        return (StatusCode::RANGE_NOT_SATISFIABLE, "out of range").into_response();
    }
    let slice = full[start as usize..=end as usize].to_vec();
    let mut resp = (StatusCode::PARTIAL_CONTENT, slice).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_RANGE,
        header::HeaderValue::from_str(&format!("bytes {start}-{end}/{}", full.len())).unwrap(),
    );
    h.insert(header::ETAG, header::HeaderValue::from_str(etag).unwrap());
    h.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    resp
}

struct Rig {
    app: axum::Router,
    daemon: std::sync::Arc<Daemon>,
    dir: tempfile::TempDir,
}

async fn rig() -> (Rig, SocketAddr0) {
    let dir = tempfile::tempdir().unwrap();
    // A body big enough to plan as segmented (>= min_segment_bytes
    // default budget): 256 KiB of recognizable bytes.
    let body: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_c = hits.clone();
    let app = Router::new().route(
        "/f.bin",
        get(move |h: HeaderMap| {
            let body = body.clone();
            hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { serve_ranged(&h, &body, "\"v1\"").await }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let daemon = std::sync::Arc::new(
        Daemon::build(
            Some(&dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            // 16 KiB segments: the default 5 MiB floor would keep a
            // 256 KiB body single-stream (no plan rows to keep).
            // A custom floor exercises the REAL planner over a
            // small body instead.
            SegmentConfig {
                min_segment_bytes: 16 * 1024,
                max_concurrency: 4,
            },
        )
        .unwrap(),
    );
    daemon.start().await.unwrap();
    (
        Rig {
            app: daemon.router(),
            daemon,
            dir,
        },
        SocketAddr0 { addr, hits },
    )
}

struct SocketAddr0 {
    addr: std::net::SocketAddr,
    hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for Rig {
    fn drop(&mut self) {
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
) -> (StatusCode, serde_json::Value) {
    let mut req = axum::http::Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header(axum::http::header::CONTENT_TYPE, "application/json");
    }
    let req = req
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, v)
}

async fn wait_completed(app: &axum::Router, id: &str) -> serde_json::Value {
    for _ in 0..200 {
        let (st, v) = json_req(app, "GET", &format!("/tasks/{id}"), None).await;
        assert_eq!(st, StatusCode::OK, "get task: {v}");
        let status = v["status"].as_str().unwrap_or_default().to_string();
        if status == "completed" || status == "failed" {
            assert_eq!(status, "completed", "task failed: {v}");
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("task {id} never completed");
}

#[tokio::test]
async fn readd_completed_url_completes_with_zero_server_fetches() {
    let (rig, srv) = rig().await;
    let sink = rig.dir.path().join("f.bin");
    let url = format!("http://{}/f.bin", srv.addr);

    // --- first add: a real segmented download over real HTTP.
    let (st, v) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": url,
            "save_path": sink,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "add: {v}");
    let id = v["id"].as_str().unwrap().to_string();

    let done = wait_completed(&rig.app, &id).await;
    assert_eq!(
        done["received_bytes"].as_u64(),
        done["total_bytes"].as_u64(),
        "first download must be fully received: {done}"
    );
    let first_hits = srv.hits.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        first_hits >= 2,
        "segmented download needs fetches: {first_hits}"
    );
    let total = done["total_bytes"].as_u64().unwrap();
    assert_eq!(total, 256 * 1024);

    // --- #31 contract: completed segment table is served.
    let (st, segs) = json_req(&rig.app, "GET", &format!("/tasks/{id}/segments"), None).await;
    assert_eq!(st, StatusCode::OK, "segments: {segs}");
    let rows = segs.as_array().expect("bare array wire shape");
    assert!(
        !rows.is_empty(),
        "completed task must keep plan rows: {segs}"
    );
    assert!(
        rows.iter()
            .all(|s| { s["pct"].as_f64() == Some(1.0) && s["done"].as_u64() == s["len"].as_u64() }),
        "every segment done: {segs}"
    );

    // --- re-add the same (url, sink): Route 1 short-circuit.
    let (st, v2) = json_req(
        &rig.app,
        "POST",
        "/tasks",
        Some(serde_json::json!({
            "url": url,
            "save_path": sink,
        })),
    )
    .await;
    // Same-sink re-add: the old row is terminal (completed), so the
    // duplicate-active guard must NOT fire (409) — a fresh row is
    // created and instantly completes.
    assert_eq!(st, StatusCode::CREATED, "re-add: {v2}");
    let id2 = v2["id"].as_str().unwrap().to_string();
    assert_ne!(id, id2, "re-add creates a NEW row id");

    let done2 = wait_completed(&rig.app, &id2).await;
    assert_eq!(
        done2["received_bytes"].as_u64(),
        Some(total),
        "re-add reports ABSOLUTE progress (#30): {done2}"
    );

    // --- the zero-fetch guarantee: not even a probe hit the server.
    let after_hits = srv.hits.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        after_hits, first_hits,
        "re-add of a completed target must make ZERO server fetches"
    );
}
