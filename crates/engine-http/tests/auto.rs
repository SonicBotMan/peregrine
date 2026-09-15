//! `download_auto()` routing tests (M1-c2): the routing table in
//! `auto.rs` module docs, one test per row, plus the structured
//! downgrade fallback and probe-failure fallback.

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use peregrine_engine_http::{HttpEngine, SegmentConfig};
use peregrine_storage::Store;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Middleware counting GETs that carry a REAL (non-probe) Range
/// header — i.e. segment workers. The probe's confirm round sends
/// `bytes=0-0` and single-stream sends no Range at all, so this
/// counter is exactly "how many segment-worker requests hit the
/// server" (P1-4, M1-c2 R2: routing tests must discriminate the
/// taken path, not just the resulting file).
async fn count_worker_ranges(
    axum::extract::State(counter): axum::extract::State<Arc<AtomicUsize>>,
    req: Request,
    next: Next,
) -> Response {
    let is_worker_range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|r| r.starts_with("bytes=") && r != "bytes=0-0");
    if is_worker_range {
        counter.fetch_add(1, Ordering::SeqCst);
    }
    next.run(req).await
}

fn counting_app() -> (Router, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let app = ranged_app().layer(axum::middleware::from_fn_with_state(
        counter.clone(),
        count_worker_ranges,
    ));
    (app, counter)
}

fn body_bytes() -> Vec<u8> {
    (0..1000u32).map(|i| (i % 251) as u8).collect()
}

/// Closed-range server (same shape as the segment tests') with an
/// `ETag`, used both by the probe (HEAD + `Range: bytes=0-0`) and the
/// segment workers.
async fn ranged_server(headers: HeaderMap) -> Response {
    serve_ranged(&headers, &body_bytes(), "\"v1\"").await
}

async fn serve_ranged(req_headers: &HeaderMap, full: &[u8], etag: &str) -> Response {
    let Some(range) = req_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(s, e)| {
            let start = s.parse::<u64>().ok()?;
            // Open-ended "bytes=N-" (single-stream resume shape):
            // the end is the file's last byte.
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
            .insert(header::ETAG, HeaderValue::from_str(etag).unwrap());
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
        HeaderValue::from_str(&format!("bytes {start}-{end}/{}", full.len())).unwrap(),
    );
    h.insert(header::ETAG, HeaderValue::from_str(etag).unwrap());
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

/// HEAD-friendly ranged server: full method dispatch (axum `get` also
/// answers HEAD automatically with the GET handler minus body).
fn ranged_app() -> Router {
    Router::new().route("/file", get(ranged_server))
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn temp_sink(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "peregrine-auto-test-{name}-{}.bin",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn engine() -> HttpEngine {
    HttpEngine::new().unwrap()
}

fn cfg() -> SegmentConfig {
    // 250-byte floor → the 1000-byte body routes segmented; a
    // 400-byte body (floor 300) routes single-stream.
    SegmentConfig::new(250, 4)
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(u64, Option<u64>)>>,
}

impl peregrine_api::ProgressSink for Recorder {
    fn on_progress(&self, p: &peregrine_api::DownloadProgress) {
        self.events.lock().unwrap().push((p.bytes_done, p.total));
    }
}

/// Row "otherwise": ranged + sized + big enough → segmented swarm.
/// Discriminating proof (P1-4): >= 2 worker GETs with full ranges
/// (the plan is 4 segments) AND a byte-exact file.
#[tokio::test]
async fn fresh_ranged_large_routes_segmented() {
    let (app, worker_gets) = counting_app();
    let addr = spawn(app).await;
    let sink = temp_sink("seg");
    let store = Store::open_memory().unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.total_bytes, Some(1000));
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    // Completed → rows kept, terminal (#31: telemetry + re-add).
    let kept = store
        .get_task(&format!("http://{addr}/file"), &sink)
        .await
        .unwrap()
        .expect("plan rows must survive completion");
    assert!(kept.segments.iter().all(|s| s.is_complete()));
    assert!(
        worker_gets.load(Ordering::SeqCst) >= 2,
        "segmented routing must actually fan out worker GETs, saw {}",
        worker_gets.load(Ordering::SeqCst)
    );
}

/// Row "too small": same server, body below 2×min_segment_bytes →
/// single stream. Discriminating proof (P1-4): ZERO worker ranged
/// GETs — a wrongly-segmented route would have fired some before
/// downgrading.
#[tokio::test]
async fn small_file_routes_single() {
    let small: Vec<u8> = (0..400u32).map(|i| (i % 97) as u8).collect();
    let counter = Arc::new(AtomicUsize::new(0));
    let c2 = counter.clone();
    let app = Router::new()
        .route(
            "/small",
            get(move |headers: HeaderMap| {
                let small = small.clone();
                async move { serve_ranged(&headers, &small, "\"s1\"").await }
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            counter,
            count_worker_ranges,
        ));
    let addr = spawn(app).await;
    let sink = temp_sink("small");
    let store = Store::open_memory().unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url: format!("http://{addr}/small"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.total_bytes, Some(400));
    let expect: Vec<u8> = (0..400u32).map(|i| (i % 97) as u8).collect();
    assert_eq!(std::fs::read(&sink).unwrap(), expect);
    assert_eq!(
        c2.load(Ordering::SeqCst),
        0,
        "below the segmented floor, no worker GET may fire"
    );
}

/// Row "no ranges": server ignores Range → probe's confirm round
/// sees 200 → single stream, completed. Discriminating (P1-4): zero
/// worker GETs — mis-routing to segmented would fire workers first
/// (they would then be downgraded; the counter catches the mistake
/// the outcome assertions cannot).
#[tokio::test]
async fn no_ranges_routes_single() {
    let counter = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/file",
            get(|| async {
                (
                    StatusCode::OK,
                    [(&header::ACCEPT_RANGES, "none"), (&header::ETAG, "\"x\"")],
                    body_bytes(),
                )
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            counter.clone(),
            count_worker_ranges,
        ));
    let addr = spawn(app).await;
    let sink = temp_sink("no-range");
    let store = Store::open_memory().unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url: format!("http://{addr}/file"),
                sink,
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.total_bytes, Some(1000));
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "no-ranges server must never see a worker GET"
    );
}

/// Row "probe fails": HEAD answers 500 but GETs work — download must
/// still complete single-stream. Same discriminating counter as the
/// no-ranges test (P1-4).
#[tokio::test]
async fn probe_failure_falls_back_to_single() {
    let counter = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/file",
            any(|req: Request| async move {
                if req.method() == axum::http::Method::HEAD {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "no head for you").into_response();
                }
                ranged_server(req.headers().clone()).await
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            counter.clone(),
            count_worker_ranges,
        ));
    let addr = spawn(app).await;
    let sink = temp_sink("headless");
    let store = Store::open_memory().unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "probe failure must fall straight to single stream, no workers"
    );
}

/// Row "store has a row": mode stickiness — an existing task row
/// routes segmented even though the caller passed no resume/total.
#[tokio::test]
async fn store_row_sticks_to_segmented() {
    let addr = spawn(ranged_app()).await;
    let url = format!("http://{addr}/file");
    let sink = temp_sink("sticky");
    let store = Store::open_memory().unwrap();

    // Half-done segmented state (same shape as the resume test).
    let tid = store
        .upsert_task(&url, &sink, Some(1000), Some("\"v1\""))
        .await
        .unwrap();
    store
        .replace_segments(tid, &[(0, 249), (250, 499), (500, 749), (750, 999)])
        .await
        .unwrap();
    store.update_cursor(tid, 0, 250).await.unwrap();
    // Sparse full-length sink, first 250 bytes real.
    std::fs::write(&sink, body_bytes()).unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url,
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // Resumed segmented: only 750 new bytes.
    assert_eq!(out.bytes_written, 750, "sticky segmented resume");
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
}

/// Row "resume context": a single-stream partial routes single even
/// though the server would happily segment.
#[tokio::test]
async fn resume_context_routes_single() {
    let addr = spawn(ranged_app()).await;
    let sink = temp_sink("resume-single");
    let store = Store::open_memory().unwrap();
    // Dense 350-byte prefix — the single-stream partial shape.
    std::fs::write(&sink, &body_bytes()[..350]).unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: Some(peregrine_api::ResumeContext {
                    start_offset: 350,
                    validator: Some(peregrine_api::IfRangeValidator::StrongEtag("\"v1\"".into())),
                }),
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // Single-stream resume: 650 NEW bytes appended at the frontier.
    // (outcome.bytes_written counts this session's writes; progress
    // events carry the cumulative resume_offset + written.)
    assert_eq!(out.bytes_written, 650, "single-stream resume tail");
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    assert!(
        store
            .get_task(&format!("http://{addr}/file"), &sink)
            .await
            .unwrap()
            .is_none(),
        "single stream never touches the segment store"
    );
}

/// Downgrade fallback: probe says ranged (confirm GET honors
/// bytes=0-0), but EVERY later ranged GET is answered 200 — the swarm
/// signals SingleStreamRequired and the router restarts single-stream,
/// producing a complete correct file.
#[tokio::test]
async fn downgrade_restarts_single_stream() {
    // Count Range requests: the probe's confirm (bytes=0-0) is
    // honored; full-range worker GETs are answered 200.
    let served_full = Arc::new(AtomicUsize::new(0));
    let counter = served_full.clone();
    let app = Router::new().route(
        "/file",
        any(move |req: Request| {
            let counter = counter.clone();
            async move {
                if req.method() == axum::http::Method::HEAD {
                    return (
                        StatusCode::OK,
                        [(&header::ACCEPT_RANGES, "bytes"), (&header::ETAG, "\"v1\"")],
                        "",
                    )
                        .into_response();
                }
                let wants_one_byte = req
                    .headers()
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|r| r == "bytes=0-0");
                if wants_one_byte {
                    // Probe confirm round: honest 206.
                    let mut resp =
                        (StatusCode::PARTIAL_CONTENT, vec![body_bytes()[0]]).into_response();
                    resp.headers_mut().insert(
                        header::CONTENT_RANGE,
                        HeaderValue::from_static("bytes 0-0/1000"),
                    );
                    resp.headers_mut()
                        .insert(header::ETAG, HeaderValue::from_static("\"v1\""));
                    return resp;
                }
                // Worker GET with a full range: the betrayal — plain 200.
                counter.fetch_add(1, Ordering::SeqCst);
                (StatusCode::OK, body_bytes()).into_response()
            }
        }),
    );
    let addr = spawn(app).await;
    let sink = temp_sink("downgrade");
    let store = Store::open_memory().unwrap();

    let out = engine()
        .download_auto(
            peregrine_api::DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed, "downgrade must end in a complete file");
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    assert!(
        served_full.load(Ordering::SeqCst) >= 1,
        "the swarm must have actually been betrayed before downgrading"
    );
}

/// B36-R2 P2-3 (key drift, fixed): probe redirects `/file` → `/real`;
/// the segmented attempt on `/real` is betrayed (200s) and downgrades;
/// the restarted single stream must key its validator row on the
/// CALLER's URL (`/file`), because every read side (scheduler
/// resume, download_auto routing) looks the row up by the original
/// URL. Before the fix the row landed under `/real` — written,
/// flushes, and is unreachable forever (validator-less resume).
#[tokio::test]
async fn downgrade_validator_row_keys_original_url() {
    let body = body_bytes();
    let real = Router::new().route(
        "/real",
        any(move |req: Request| {
            let body = body.clone();
            async move {
                if req.method() == axum::http::Method::HEAD {
                    return (
                        StatusCode::OK,
                        [
                            (&header::ACCEPT_RANGES, "bytes"),
                            (&header::ETAG, "\"v1\""),
                            (&header::CONTENT_LENGTH, "1000"),
                        ],
                        "",
                    )
                        .into_response();
                }
                let confirm = req
                    .headers()
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|r| r == "bytes=0-0");
                if confirm {
                    let mut resp =
                        (StatusCode::PARTIAL_CONTENT, body[..1].to_vec()).into_response();
                    let h = resp.headers_mut();
                    h.insert(
                        header::CONTENT_RANGE,
                        HeaderValue::from_static("bytes 0-0/1000"),
                    );
                    h.insert(header::ETAG, HeaderValue::from_static("\"v1\""));
                    return resp;
                }
                if req.headers().contains_key(header::RANGE) {
                    // Worker betrayal — triggers SingleStreamRequired.
                    let mut resp = (StatusCode::OK, body.clone()).into_response();
                    resp.headers_mut()
                        .insert(header::ETAG, HeaderValue::from_static("\"v1\""));
                    return resp;
                }
                // Bare GET on /real answers 200 like a real mirror —
                // so a pre-fix restart keyed on /real FAILS the row
                // assertion itself (not a 404 artifact) (R2 P3-1).
                let mut resp = (StatusCode::OK, body.clone()).into_response();
                resp.headers_mut()
                    .insert(header::ETAG, HeaderValue::from_static("\"v1\""));
                resp
            }
        }),
    );
    let body2 = body_bytes();
    let app = Router::new()
        .route(
            "/file",
            // The caller's URL: HEAD redirects to /real (probe chase);
            // the downgrade restart's GET lands here for the full body.
            any(move |req: Request| {
                let body = body2.clone();
                async move {
                    if req.method() == axum::http::Method::HEAD {
                        return (StatusCode::FOUND, [(&header::LOCATION, "/real")], "")
                            .into_response();
                    }
                    let mut resp = (StatusCode::OK, body.clone()).into_response();
                    resp.headers_mut()
                        .insert(header::ETAG, HeaderValue::from_static("\"v1\""));
                    resp
                }
            }),
        )
        .merge(real);
    let addr = spawn(app).await;
    let sink = temp_sink("downgrade-key");
    let store = Store::open_memory().unwrap();
    // The engine writes validator rows only when built with the
    // store (daemon wiring injects it; plain `new()` skips writes).
    let eng = HttpEngine::new()
        .unwrap()
        .with_validator_store(store.clone());
    let original = format!("http://{addr}/file");
    let final_url = format!("http://{addr}/real");

    let out = eng
        .download_auto(
            peregrine_api::DownloadJob {
                url: original.clone(),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Recorder::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    // THE row lives under the ORIGINAL url, validator-only shape.
    let row = store
        .get_task(&original, &sink)
        .await
        .unwrap()
        .expect("validator row must be keyed on the original URL");
    assert_eq!(row.total, None, "validator-only row never carries a total");
    assert_eq!(row.etag.as_deref(), Some("\"v1\""));
    // And no orphan under the probe's final URL.
    assert!(
        store.get_task(&final_url, &sink).await.unwrap().is_none(),
        "no validator row may be keyed on the final URL"
    );
}

/// A live (never-cancelled) token for tests that don't exercise cancellation.
fn token() -> CancellationToken {
    CancellationToken::new()
}
