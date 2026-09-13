//! `download_segmented()` against a local axum mock: static planning,
//! concurrent workers, cursor-persisted resume, validator-change wipe,
//! two-ended 206 refusal (B20), and the 200-downgrade hint.

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use peregrine_api::{
    ApiError, DownloadJob, DownloadProgress, IfRangeValidator, ProgressSink, ResumeContext,
};
use peregrine_engine_http::{HttpEngine, SegmentConfig};
use peregrine_storage::Store;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn body_bytes() -> Vec<u8> {
    (0..1000u32).map(|i| (i % 251) as u8).collect()
}

/// Full closed-range handler: parses `bytes=S-E`, serves exactly
/// [S, E] with a matching two-ended Content-Range — the shape a
/// spec-correct server answers a segment worker with.
async fn closed_ranged(headers: HeaderMap) -> Response {
    serve_closed(&headers, &body_bytes(), "v1").await
}

async fn serve_closed(req_headers: &HeaderMap, full: &[u8], etag: &str) -> Response {
    let Some(range) = req_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(s, e)| Some((s.parse::<u64>().ok()?, e.parse::<u64>().ok()?)))
    else {
        // No (or malformed) Range: full 200.
        let mut resp = (StatusCode::OK, full.to_vec()).into_response();
        resp.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{etag}\"")).unwrap(),
        );
        return resp;
    };
    let (start, end) = range;
    if start >= full.len() as u64 || end >= full.len() as u64 || end < start {
        let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "out of range").into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes */{}", full.len())).unwrap(),
        );
        return resp;
    }
    let slice = full[start as usize..=end as usize].to_vec();
    let mut resp = (StatusCode::PARTIAL_CONTENT, slice).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{}", full.len())).unwrap(),
    );
    h.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{etag}\"")).unwrap(),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

/// Ignores the requested END: answers `bytes S-(T-1)/T` — the shape
/// that MUST trip the two-ended check (B20).
async fn open_ended_206(headers: HeaderMap) -> Response {
    let full = body_bytes();
    let Some(start) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split('-').next())
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return (StatusCode::OK, full).into_response();
    };
    let slice = full[start as usize..].to_vec();
    let mut resp = (StatusCode::PARTIAL_CONTENT, slice).into_response();
    resp.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!(
            "bytes {start}-{}/{}",
            full.len() as u64 - 1,
            full.len()
        ))
        .unwrap(),
    );
    resp
}

/// Always a full 200 — server stopped honoring Range mid-flight.
async fn always_200() -> Response {
    (StatusCode::OK, body_bytes()).into_response()
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(u64, Option<u64>)>>,
}

impl ProgressSink for Recorder {
    fn on_progress(&self, p: &DownloadProgress) {
        self.events.lock().unwrap().push((p.bytes_done, p.total));
    }
}

impl Recorder {
    fn last(&self) -> (u64, Option<u64>) {
        *self.events.lock().unwrap().last().unwrap()
    }
}

async fn spawn(routes: Vec<(&'static str, axum::routing::MethodRouter)>) -> SocketAddr {
    let mut app = Router::new();
    for (path, method) in routes {
        app = app.route(path, method);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn temp_sink(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "peregrine-seg-test-{name}-{}.bin",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn engine() -> HttpEngine {
    HttpEngine::new().unwrap()
}

/// Standard segmented call: 1000-byte body, 250-byte segments → K=4.
fn std_cfg() -> SegmentConfig {
    SegmentConfig::new(250, 4)
}

fn job(url: String, sink: PathBuf) -> DownloadJob {
    DownloadJob {
        url,
        sink,
        resume: None,
        expected_total: Some(1000),
    }
}

#[tokio::test]
async fn segmented_download_assembles_exact_file() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let sink = temp_sink("happy");
    let store = Store::open_memory().unwrap();
    let rec = Arc::new(Recorder::default());

    let out = engine()
        .download_segmented(
            job(format!("http://{addr}/file"), sink.clone()),
            &std_cfg(),
            &store,
            rec.clone(),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.total_bytes, Some(1000));
    assert_eq!(out.bytes_written, 1000);
    let disk = std::fs::read(&sink).unwrap();
    assert_eq!(disk, body_bytes(), "segments must assemble byte-exact");
    // Task row dropped on completion — file on disk is the truth.
    assert!(
        store
            .get_task(&format!("http://{addr}/file"), &sink)
            .await
            .unwrap()
            .is_none()
    );
    // Final progress event is exactly the total.
    assert_eq!(rec.last(), (1000, Some(1000)));
}

#[tokio::test]
async fn workers_run_concurrently_within_budget() {
    let in_flight = Arc::new(AtomicI64::new(0));
    let peak = Arc::new(AtomicU64::new(0));
    let inflight_c = in_flight.clone();
    let peak_c = peak.clone();
    let handler = move |headers: HeaderMap| {
        let inflight = inflight_c.clone();
        let peak = peak_c.clone();
        async move {
            let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now as u64, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            inflight.fetch_sub(1, Ordering::SeqCst);
            serve_closed(&headers, &body_bytes(), "v1").await
        }
    };
    let addr = spawn(vec![("/file", get(handler))]).await;
    let sink = temp_sink("concurrency");
    let store = Store::open_memory().unwrap();

    engine()
        .download_segmented(
            job(format!("http://{addr}/file"), sink),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    let peak = peak.load(Ordering::SeqCst);
    assert!(peak > 1, "workers must overlap (peak {peak})");
    assert!(peak <= 4, "concurrency budget respected (peak {peak})");
}

#[tokio::test]
async fn resume_continues_from_persisted_cursors() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let sink = temp_sink("resume");
    let url = format!("http://{addr}/file");
    let store = Store::open_memory().unwrap();

    // Preexisting half-done state: 4 segments, seg0 complete, seg1
    // at 100/250, others untouched — 350 bytes confirmed on disk.
    let tid = store
        .upsert_task(&url, &sink, Some(1000), Some("\"v1\""))
        .await
        .unwrap();
    store
        .replace_segments(tid, &[(0, 249), (250, 499), (500, 749), (750, 999)])
        .await
        .unwrap();
    store.update_cursor(tid, 0, 250).await.unwrap();
    store.update_cursor(tid, 1, 100).await.unwrap();

    // Sink as it would look after a crash post-preallocation: full
    // length on disk (sparse tail), first 350 bytes real content,
    // remainder zeros. P0-1 (M1-c1 R2) requires sink.len() == total
    // for a resume to be trusted.
    std::fs::write(&sink, body_bytes()).unwrap();

    let rec = Arc::new(Recorder::default());
    let mut j = job(url.clone(), sink.clone());
    j.resume = Some(ResumeContext {
        start_offset: 0, // unused by the segmented path; validator is what matters
        validator: Some(IfRangeValidator::StrongEtag("\"v1\"".into())),
    });

    let out = engine()
        .download_segmented(
            j,
            &std_cfg(),
            &store,
            rec.clone(),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    let disk = std::fs::read(&sink).unwrap();
    assert_eq!(disk, body_bytes());
    assert_eq!(out.bytes_written, 650, "only the missing 650 bytes fetched");
    // Initial progress event reported the resumed amount.
    assert!(rec.events.lock().unwrap().contains(&(350, Some(1000))));
}

#[tokio::test]
async fn validator_change_wipes_and_replans() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let url = format!("http://{addr}/file");
    let sink = temp_sink("wipe");
    let store = Store::open_memory().unwrap();

    // Stored etag says "v9" but the probe now serves "v1" — the old
    // segment set is stale and must be wiped, not resumed.
    let tid = store
        .upsert_task(&url, &sink, Some(1000), Some("\"v9\""))
        .await
        .unwrap();
    store.replace_segments(tid, &[(0, 999)]).await.unwrap();
    store.update_cursor(tid, 0, 600).await.unwrap();

    let mut j = job(url, sink.clone());
    j.resume = Some(ResumeContext {
        start_offset: 0,
        validator: Some(IfRangeValidator::StrongEtag("\"v1\"".into())),
    });
    let out = engine()
        .download_segmented(
            j,
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert_eq!(out.bytes_written, 1000, "stale cursors must not be trusted");
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
}

#[tokio::test]
async fn two_ended_206_mismatch_is_refused() {
    let addr = spawn(vec![("/file", get(open_ended_206))]).await;
    let sink = temp_sink("mismatch");
    let store = Store::open_memory().unwrap();

    let err = engine()
        .download_segmented(
            job(format!("http://{addr}/file"), sink.clone()),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("refusing mismatched bytes"), "got: {msg}");
    // Nothing glued, task row intact for a retry.
    assert!(
        store
            .get_task(&format!("http://{addr}/file"), &sink)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn ignored_range_hints_single_stream_downgrade() {
    let addr = spawn(vec![("/file", get(always_200))]).await;
    let sink = temp_sink("downgrade");
    let store = Store::open_memory().unwrap();

    let err = engine()
        .download_segmented(
            job(format!("http://{addr}/file"), sink),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("single stream required"),
        "structured downgrade signal, got: {msg}"
    );
    // It IS the dedicated variant, not a stringly-typed network error.
    assert!(matches!(err, ApiError::SingleStreamRequired { .. }));
}

#[tokio::test]
async fn unknown_total_is_rejected_upfront() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let sink = temp_sink("unknown");
    let store = Store::open_memory().unwrap();
    let mut j = job(format!("http://{addr}/file"), sink);
    j.expected_total = None;

    let err = engine()
        .download_segmented(
            j,
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("known total"));
}

/// Shared prep: a half-done task row (seg0 complete, seg1 at 100,
/// rest untouched → 350 confirmed) — the same shape the resume test
/// uses, parameterized by what we do to the sink file.
async fn half_done_task(
    store: &Store,
    url: &str,
    sink: &std::path::Path,
) -> peregrine_storage::TaskId {
    let tid = store
        .upsert_task(url, sink, Some(1000), Some("\"v1\""))
        .await
        .unwrap();
    store
        .replace_segments(tid, &[(0, 249), (250, 499), (500, 749), (750, 999)])
        .await
        .unwrap();
    store.update_cursor(tid, 0, 250).await.unwrap();
    store.update_cursor(tid, 1, 100).await.unwrap();
    tid
}

fn resume_job(url: String, sink: PathBuf) -> DownloadJob {
    let mut j = job(url, sink);
    j.resume = Some(ResumeContext {
        start_offset: 0,
        validator: Some(IfRangeValidator::StrongEtag("\"v1\"".into())),
    });
    j
}

/// P0-1 (M1-c1 R2): a missing partial file (temp cleaner, user
/// cleanup) must auto-recover by replanning from zero — the common
/// case — instead of gluing cursors onto a nonexistent file.
#[tokio::test]
async fn missing_sink_auto_replans_from_zero() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let url = format!("http://{addr}/file");
    let sink = temp_sink("missing-sink");
    let store = Store::open_memory().unwrap();
    half_done_task(&store, &url, &sink).await;
    // Sink deliberately NOT created.

    let out = engine()
        .download_segmented(
            resume_job(url, sink.clone()),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert_eq!(
        out.bytes_written, 1000,
        "full refetch after missing partial"
    );
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
}

/// P0-1 (M1-c1 R2): a sink whose length disagrees with the plan is
/// outside interference we refuse to paper over — visible error,
/// nothing glued.
#[tokio::test]
async fn wrong_length_sink_is_refused() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let url = format!("http://{addr}/file");
    let sink = temp_sink("wrong-len");
    let store = Store::open_memory().unwrap();
    half_done_task(&store, &url, &sink).await;
    // 350 bytes: the OLD frontier shape — no longer a valid resume.
    std::fs::write(&sink, &body_bytes()[..350]).unwrap();

    let err = engine()
        .download_segmented(
            resume_job(url.clone(), sink.clone()),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("resume mismatch"), "got: {msg}");
    // Cursors intact — the user may fix the file and retry.
    let t = store.get_task(&url, &sink).await.unwrap().unwrap();
    assert_eq!(t.segments.iter().map(|s| s.done).sum::<u64>(), 350);
}

/// P1-2 (M1-c1 R2): a server serving MORE bytes than the requested
/// closed range must never spill into the next segment's territory.
#[tokio::test]
async fn over_serving_worker_is_refused() {
    // Serves [start, end+100] with a Content-Range that honestly
    // echoes [start, end]: headers pass the two-ended check, the
    // BODY is the lie — exactly the shape the over-serve guard
    // exists for.
    async fn over_server(headers: HeaderMap) -> Response {
        let full = body_bytes();
        let Some((start, end)) = headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(s, e)| Some((s.parse::<u64>().ok()?, e.parse::<u64>().ok()?)))
        else {
            return (StatusCode::OK, full).into_response();
        };
        let served_end = (end + 100).min(full.len() as u64 - 1);
        let slice = full[start as usize..=served_end as usize].to_vec();
        let mut resp = (StatusCode::PARTIAL_CONTENT, slice).into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", full.len())).unwrap(),
        );
        resp
    }

    let addr = spawn(vec![("/file", get(over_server))]).await;
    let sink = temp_sink("over-serve");
    let store = Store::open_memory().unwrap();

    let err = engine()
        .download_segmented(
            job(format!("http://{addr}/file"), sink.clone()),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = format!("{err}");
    assert!(msg.contains("over-serve"), "got: {msg}");
}

/// P1-3 (M1-c1 R2): a server that IGNORES If-Range (answers 206 even
/// for changed bytes) is caught by the served-etag mismatch — the
/// swarm restarts from zero and lands on the NEW content.
#[tokio::test]
async fn served_etag_mismatch_triggers_restart() {
    // Ignores If-Range entirely; always serves "v2" content with a
    // matching "v2" etag on 206s.
    async fn sneaky_v2(headers: HeaderMap) -> Response {
        let mut full = body_bytes();
        full[999] = 0xAB; // one byte changed — glueing would corrupt
        serve_closed(&headers, &full, "v2").await
    }

    let addr = spawn(vec![("/file", get(sneaky_v2))]).await;
    let url = format!("http://{addr}/file");
    let sink = temp_sink("etag-restart");
    let store = Store::open_memory().unwrap();
    half_done_task(&store, &url, &sink).await;
    std::fs::write(&sink, body_bytes()).unwrap();

    let mut expect = body_bytes();
    expect[999] = 0xAB;
    let out = engine()
        .download_segmented(
            resume_job(url, sink.clone()),
            &std_cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // First attempt restarts (restart=true), second runs validator-less
    // from zero and completes against the v2 content.
    assert_eq!(out.bytes_written, 1000, "restart refetches everything");
    assert_eq!(
        std::fs::read(&sink).unwrap(),
        expect,
        "final file is v2 bytes"
    );
}

/// P1-5 (M1-c1 R2): every progress event carries the FILE total,
/// never a segment end — bytes_done is cumulative for the file.
#[tokio::test]
async fn progress_total_is_always_file_total() {
    let addr = spawn(vec![("/file", get(closed_ranged))]).await;
    let url = format!("http://{addr}/file");
    let sink = temp_sink("progress-total");
    let store = Store::open_memory().unwrap();
    half_done_task(&store, &url, &sink).await;
    std::fs::write(&sink, body_bytes()).unwrap();

    let rec = Arc::new(Recorder::default());
    let out = engine()
        .download_segmented(
            resume_job(url, sink.clone()),
            &std_cfg(),
            &store,
            rec.clone(),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();
    assert!(out.completed);

    let events = rec.events.lock().unwrap();
    assert!(!events.is_empty());
    for (i, (done, total)) in events.iter().enumerate() {
        assert_eq!(
            *total,
            Some(1000),
            "event {i} ({done:?}/{total:?}) must use file total"
        );
    }
    // Monotonic across the whole file, across segments.
    for w in events.windows(2) {
        assert!(
            w[0].0 <= w[1].0,
            "bytes_done must be monotonic: {:?}",
            events
        );
    }
}

/// A live (never-cancelled) token for tests that don't exercise cancellation.
fn token() -> CancellationToken {
    CancellationToken::new()
}

// --------------------------------------------------------------------
// M2-b: cooperative cancellation (CancellationToken)

/// Slow closed-range handler: parses `bytes=S-E` and drips the slice
/// one 32B chunk per 25ms — workers sit mid-body long enough to cancel.
async fn slow_closed(headers: HeaderMap) -> Response {
    use axum::body::Bytes;
    use futures::stream::StreamExt as _;
    let full = body_bytes();
    let Some((start, end)) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(s, e)| Some((s.parse::<u64>().ok()?, e.parse::<u64>().ok()?)))
    else {
        return (StatusCode::OK, full).into_response();
    };
    let slice = full[start as usize..=end as usize].to_vec();
    let chunks: VecDeque<Result<Bytes, std::io::Error>> = slice
        .chunks(32)
        .map(Bytes::copy_from_slice)
        .map(Ok)
        .collect();
    let stream = futures::stream::iter(chunks).then(|c| async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        c
    });
    let mut resp = (
        StatusCode::PARTIAL_CONTENT,
        axum::body::Body::from_stream(stream),
    )
        .into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{}", full.len())).unwrap(),
    );
    h.insert(header::ETAG, HeaderValue::from_static("\"v1\""));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

#[tokio::test]
async fn cancel_mid_swarm_keeps_cursors_and_resumes_cleanly() {
    let addr = spawn(vec![("/file", get(slow_closed))]).await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("cancel-swarm");
    let store = Store::open_memory().unwrap();
    let cfg = SegmentConfig {
        min_segment_bytes: 100,
        max_concurrency: 4,
    };

    let token = CancellationToken::new();
    let t2 = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        t2.cancel();
    });

    let url = format!("http://{addr}/file");
    let job = DownloadJob {
        url: url.clone(),
        sink: sink.clone(),
        resume: Some(ResumeContext {
            start_offset: 0,
            validator: Some(IfRangeValidator::StrongEtag("v1".into())),
        }),
        expected_total: Some(1000),
    };
    let err = engine
        .download_segmented(
            job,
            &cfg,
            &store,
            Arc::new(Recorder::default()),
            token,
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Cancelled), "got {err:?}");

    // The task row survives with a known total — that IS the resume
    // state; wiping it on pause would throw away every flushed cursor.
    let row = store
        .get_task(&url, &sink)
        .await
        .unwrap()
        .expect("row kept");
    assert_eq!(row.total, Some(1000));
    let done: u64 = row.segments.iter().map(|s| s.done).sum();
    assert!(
        done < 1000,
        "cancelled swarm cannot be complete (done={done})"
    );
    // Preallocated sparse sink at full size.
    assert_eq!(std::fs::metadata(&sink).unwrap().len(), 1000);

    // The kill-shot for ghost workers: a clean resume after cancel
    // completes the file exactly once. Overlapping byte writes from
    // a stray worker would surface here as mismatched bytes.
    let out = engine
        .download_segmented(
            DownloadJob {
                url: url.clone(),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 0,
                    validator: Some(IfRangeValidator::StrongEtag("v1".into())),
                }),
                expected_total: Some(1000),
            },
            &cfg,
            &store,
            Arc::new(Recorder::default()),
            CancellationToken::new(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();
    assert!(out.completed);
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
}

// --------------------------------------------------------------------
// M2-b R2 P0-1 regression: no ghost worker may outlive a cancelled
// swarm. Observable via the mock: count in-flight requests. A leaked
// worker keeps dripping (25ms/chunk) well past the joiner's return;
// a properly aborted one drives the count to zero quickly.

/// Shared in-flight request counter for leak detection.
static SLOW_ACTIVE: AtomicI64 = AtomicI64::new(0);

async fn slow_closed_counted(headers: HeaderMap) -> Response {
    SLOW_ACTIVE.fetch_add(1, Ordering::SeqCst);
    let resp = slow_closed(headers).await;
    SLOW_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    resp
}

#[tokio::test]
async fn cancelled_swarm_leaves_no_ghost_worker() {
    let addr = spawn(vec![("/file", get(slow_closed_counted))]).await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("ghost-check");
    let store = Store::open_memory().unwrap();
    let cfg = SegmentConfig {
        min_segment_bytes: 100,
        max_concurrency: 4,
    };

    let token = CancellationToken::new();
    let t2 = token.clone();
    tokio::spawn(async move {
        // Long enough that workers are mid-body (each 100B segment
        // drips for ~75ms at 25ms/32B), short of completion.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        t2.cancel();
    });

    let job = DownloadJob {
        url: format!("http://{addr}/file"),
        sink: sink.clone(),
        resume: None,
        expected_total: Some(1000),
    };
    let err = engine
        .download_segmented(
            job,
            &cfg,
            &store,
            Arc::new(Recorder::default()),
            token,
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Cancelled), "got {err:?}");

    // The joiner has returned; every worker must already be aborted.
    // Poll briefly: a ghost would hold the count above zero while it
    // finishes its ~700ms of remaining drips.
    for _ in 0..40 {
        if SLOW_ACTIVE.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        SLOW_ACTIVE.load(Ordering::SeqCst),
        0,
        "a segment worker outlived the cancelled download (ghost)"
    );
}
