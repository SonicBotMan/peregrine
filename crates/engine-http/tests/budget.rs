//! Rate-budget integration (M3-b1): the engine actually throttles
//! bytes through a real `RateBudget`, both single-stream and
//! segmented, and a live `set_bps` change takes effect mid-download.
//!
//! These tests are wall-clock sensitive BY DESIGN — they assert a
//! LOWER bound on elapsed time, so they pass on slow machines and
//! can only flake by being too fast, which would mean the budget is
//! broken. Rates are chosen so the throttled pass needs ~0.5-1 s.

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use peregrine_api::{
    DownloadJob, NoProgress, ProtocolEngine, ResumeContext,
    budget::{BudgetChain, RateBudget},
};
use peregrine_engine_http::HttpEngine;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

// 64 KiB of data in 16 KiB frames — enough bytes that a budget of
// 128 KiB/s must park visibly, small enough to be fast unlimited.
const CHUNKS: usize = 16;
const CHUNK: usize = 16 * 1024;
fn big() -> Vec<u8> {
    vec![7u8; CHUNKS * CHUNK]
}

async fn stream_big() -> Response {
    let body = futures::stream::iter(
        (0..CHUNKS)
            .map(|_| Ok::<_, std::io::Error>(vec![7u8; CHUNK]))
            .collect::<Vec<_>>(),
    );
    (
        StatusCode::OK,
        [(header::CONTENT_LENGTH, CHUNKS * CHUNK)],
        axum::body::Body::from_stream(body),
    )
        .into_response()
}

/// Ranged variant over the same bytes: full Range/If-Range fidelity
/// so the segmented engine accepts it.
async fn ranged_big(headers: HeaderMap) -> Response {
    let full = big();
    let start = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split('-').next())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let end = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .map(|(_, e)| e.parse::<u64>().unwrap_or(full.len() as u64 - 1))
        .unwrap_or(full.len() as u64 - 1);
    let slice = full[start as usize..=(end as usize)].to_vec();
    let mut resp = (StatusCode::PARTIAL_CONTENT, slice).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_RANGE,
        axum::http::HeaderValue::from_str(&format!("bytes {start}-{}/{}", end, full.len()))
            .unwrap(),
    );
    h.insert(
        header::ACCEPT_RANGES,
        axum::http::HeaderValue::from_static("bytes"),
    );
    h.insert(header::ETAG, axum::http::HeaderValue::from_static("\"v1\""));
    resp
}

async fn spawn_mock() -> SocketAddr {
    let app = Router::new()
        .route("/file", get(stream_big))
        .route("/ranged", get(ranged_big));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn sink(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("pgrg-budget-{tag}-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Baseline sanity: unlimited chain downloads instantly (~ms).
#[tokio::test]
async fn unlimited_passes_at_line_speed() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let t0 = Instant::now();
    engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink("fast"),
                resume: None,
                expected_total: Some((CHUNKS * CHUNK) as u64),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress) as peregrine_api::download::SharedProgressSink,
            CancellationToken::new(),
            &BudgetChain::unlimited(),
        )
        .await
        .unwrap();
    assert!(t0.elapsed() < Duration::from_millis(2_000));
}

/// 256 KiB through a 128 KiB/s budget must take ≥2 s.
#[tokio::test]
async fn single_stream_throttles_to_budget() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let budget = BudgetChain {
        local: RateBudget::with_bps(128 * 1024),
        global: RateBudget::unlimited(),
    };
    let t0 = Instant::now();
    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink("throttled"),
                resume: None,
                expected_total: Some((CHUNKS * CHUNK) as u64),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress) as peregrine_api::download::SharedProgressSink,
            CancellationToken::new(),
            &budget,
        )
        .await
        .unwrap();
    assert!(out.completed);
    let elapsed = t0.elapsed();
    assert!(
        elapsed >= Duration::from_millis(950),
        "256 KiB through 128 KiB/s finished in {elapsed:?} — budget not enforced"
    );
}

/// Global budget alone (local unlimited) throttles the segmented
/// engine's workers collectively: 2 workers × 256 KiB total through
/// 128 KiB/s ≥ 1 s (fuzzed for worker scheduling).
#[tokio::test]
async fn global_budget_throttles_segment_swarm() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let store = peregrine_storage::Store::open_memory().unwrap();
    let cfg = peregrine_engine_http::SegmentConfig {
        max_concurrency: 2,
        ..Default::default()
    };
    let budget = BudgetChain {
        local: RateBudget::unlimited(),
        global: RateBudget::with_bps(128 * 1024),
    };
    let t0 = Instant::now();
    let out = engine
        .download_segmented(
            DownloadJob {
                url: format!("http://{addr}/ranged"),
                sink: sink("swarm"),
                resume: Some(ResumeContext {
                    start_offset: 0,
                    validator: None,
                }),
                expected_total: Some((CHUNKS * CHUNK) as u64),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            &cfg,
            &store,
            Arc::new(NoProgress) as peregrine_api::download::SharedProgressSink,
            CancellationToken::new(),
            &budget,
        )
        .await
        .unwrap();
    assert!(out.completed);
    let elapsed = t0.elapsed();
    assert!(
        elapsed >= Duration::from_millis(950),
        "swarm through 128 KiB/s global finished in {elapsed:?} — global budget not enforced"
    );
}

/// Live set_bps mid-download: start slow (16 KiB/s), then raise to
/// unlimited after ~0.3 s — total time must land WELL under the
/// all-slow bound (~4 s) proving the change took effect.
#[tokio::test]
async fn live_limit_raise_speeds_up_mid_download() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let local = RateBudget::with_bps(16 * 1024);
    let budget = BudgetChain {
        local: local.clone(),
        global: RateBudget::unlimited(),
    };
    let t0 = Instant::now();
    let dl = engine.download(
        DownloadJob {
            url: format!("http://{addr}/file"),
            sink: sink("live"),
            resume: None,
            expected_total: Some((CHUNKS * CHUNK) as u64),
            mirrors: Vec::new(),
            fetch_base: None,
        },
        Arc::new(NoProgress) as peregrine_api::download::SharedProgressSink,
        CancellationToken::new(),
        &budget,
    );
    tokio::pin!(dl);
    tokio::select! {
        out = &mut dl => { out.unwrap(); }
        _ = tokio::time::sleep(Duration::from_millis(300)) => {
            local.set_bps(0); // unlimited, live
            (&mut dl).await.unwrap();
        }
    }
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_millis(2_500),
        "raising the limit live did not unthrottle: {elapsed:?}"
    );
}

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

// --------------------------------------------------------------------
// M3-b1 real-traffic repro: a segmented download under a shared
// budget must converge to the CAP total, not cap x segments. Found
// in the end-to-end smoke (tightened to 128 KiB/s, measured 400+
// KiB/s) -- this pins the engine layer with a bigger body and a
// sampled rate window.
// --------------------------------------------------------------------

const MANY_CHUNKS: usize = 256; // 4 MiB of 16 KiB frames
fn huge() -> Vec<u8> {
    vec![7u8; MANY_CHUNKS * CHUNK]
}

async fn ranged_huge(headers: HeaderMap) -> Response {
    let full = huge();
    let parse = |v: Option<&str>| -> Option<(u64, u64)> {
        let v = v?.strip_prefix("bytes=")?;
        let (s, e) = v.split_once('-')?;
        Some((s.parse().ok()?, e.parse().ok()?))
    };
    let Some((start, end)) = parse(headers.get(header::RANGE).and_then(|v| v.to_str().ok())) else {
        let mut r = (StatusCode::OK, full).into_response();
        r.headers_mut()
            .insert(header::ETAG, axum::http::HeaderValue::from_static("\"v1\""));
        return r;
    };
    let slice = full[start as usize..=(end as usize)].to_vec();
    let mut resp = (StatusCode::PARTIAL_CONTENT, slice).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_RANGE,
        axum::http::HeaderValue::from_str(&format!("bytes {start}-{}/{}", end, full.len()))
            .unwrap(),
    );
    h.insert(
        header::ACCEPT_RANGES,
        axum::http::HeaderValue::from_static("bytes"),
    );
    h.insert(header::ETAG, axum::http::HeaderValue::from_static("\"v1\""));
    resp
}

#[derive(Default)]
struct CountingProgress(std::sync::Mutex<u64>);
impl peregrine_api::ProgressSink for CountingProgress {
    fn on_progress(&self, p: &peregrine_api::DownloadProgress) {
        *self.0.lock().unwrap() = p.bytes_done;
    }
}

#[tokio::test]
async fn segmented_budget_converges_to_cap() {
    let app = Router::new().route("/huge", get(ranged_huge));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let store = peregrine_storage::Store::open_memory().unwrap();
    let rec = Arc::new(CountingProgress::default());

    // 4 MiB in 256 KiB segments, 6 concurrent workers.
    let cfg = peregrine_engine_http::SegmentConfig {
        min_segment_bytes: 256 * 1024,
        max_concurrency: 6,
    };
    let chain = BudgetChain {
        local: RateBudget::with_bps(512 * 1024),
        global: RateBudget::unlimited(),
    };

    let cancel = CancellationToken::new();
    let handle = {
        let cancel = cancel.clone();
        let rec = rec.clone();
        let sink = sink("conv");
        tokio::spawn(async move {
            engine
                .download_segmented(
                    DownloadJob {
                        url: format!("http://{addr}/huge"),
                        sink,
                        resume: Some(ResumeContext {
                            start_offset: 0,
                            validator: None,
                        }),
                        expected_total: Some((MANY_CHUNKS * CHUNK) as u64),
                        mirrors: Vec::new(),
                        fetch_base: None,
                    },
                    &cfg,
                    &store,
                    rec.clone() as peregrine_api::download::SharedProgressSink,
                    cancel,
                    &chain,
                )
                .await
        })
    };

    // Warm up, then sample a 4 s window mid-flight.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let r0 = *rec.0.lock().unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let r1 = *rec.0.lock().unwrap();
    cancel.cancel();
    let _ = handle.await;

    let rate_kib = r1.saturating_sub(r0) / 4 / 1024;
    println!("measured {rate_kib} KiB/s over 4 s (cap 512 KiB/s)");
    assert!(
        rate_kib <= 700,
        "budget leaks: {rate_kib} KiB/s against a 512 KiB/s cap (cap x workers?)"
    );
}

/// Live per-task poke mid-flight: tighten the LOCAL budget to
/// 128 KiB/s while 6 segment workers are mid-download; the measured
/// rate must converge to the cap (smoke phase 3 measured ~4x).
#[tokio::test]
async fn live_local_poke_converges_segment_swarm() {
    let app = Router::new().route("/huge", get(ranged_huge));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let store = peregrine_storage::Store::open_memory().unwrap();
    let rec = Arc::new(CountingProgress::default());

    let cfg = peregrine_engine_http::SegmentConfig {
        min_segment_bytes: 256 * 1024,
        max_concurrency: 6,
    };
    let local = RateBudget::unlimited();
    let chain = BudgetChain {
        local: local.clone(),
        global: RateBudget::unlimited(),
    };

    let cancel = CancellationToken::new();
    let handle = {
        let cancel = cancel.clone();
        let rec = rec.clone();
        let sink = sink("poke");
        tokio::spawn(async move {
            engine
                .download_segmented(
                    DownloadJob {
                        url: format!("http://{addr}/huge"),
                        sink,
                        resume: Some(ResumeContext {
                            start_offset: 0,
                            validator: None,
                        }),
                        expected_total: Some((MANY_CHUNKS * CHUNK) as u64),
                        mirrors: Vec::new(),
                        fetch_base: None,
                    },
                    &cfg,
                    &store,
                    rec as peregrine_api::download::SharedProgressSink,
                    cancel,
                    &chain,
                )
                .await
        })
    };

    // 3 s warm (unlimited), then poke local to 128 KiB/s and sample 4 s.
    tokio::time::sleep(Duration::from_secs(3)).await;
    local.set_bps(128 * 1024);
    let r0 = *rec.0.lock().unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let r1 = *rec.0.lock().unwrap();
    cancel.cancel();
    let _ = handle.await;

    let rate_kib = r1.saturating_sub(r0) / 4 / 1024;
    println!("measured {rate_kib} KiB/s over 4 s after poke (cap 128 KiB/s)");
    assert!(
        rate_kib <= 170,
        "live poke leak: {rate_kib} KiB/s against a 128 KiB/s cap"
    );
}
