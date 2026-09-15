//! B36: single-stream resume validator persistence.
//!
//! Write side: the engine's first response persists its strong
//! validator keyed by (url, sink) as a total-NULL row (validator-only).
//! Read side: `download_auto` Route 2 feeds the stored validator into
//! the resume `If-Range` — an unchanged remote answers 206 and the
//! append proceeds; a CHANGED remote answers 200 and the truncate
//! branch rewrites from zero instead of gluing a mixed body.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use peregrine_api::{DownloadJob, ProtocolEngine, ResumeContext};
use peregrine_engine_http::{HttpEngine, SegmentConfig};
use peregrine_storage::Store;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Mutable server-side truth: which etag/body is current, plus every
/// If-Range header received (None = request had none), in order.
#[derive(Clone, Default)]
struct ServerState {
    etag: Arc<Mutex<&'static str>>,
    if_ranges: Arc<Mutex<Vec<Option<String>>>>,
}

impl ServerState {
    fn body(&self) -> Vec<u8> {
        let etag = *self.etag.lock().unwrap();
        match etag {
            "v1" => (0..1000u32).map(|i| (i % 251) as u8).collect(),
            // Same length, different bytes: the changed remote.
            _ => (0..1000u32).map(|i| ((i + 7) % 251) as u8).collect(),
        }
    }
}

async fn ranged_file(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let etag = *state.etag.lock().unwrap();
    state.if_ranges.lock().unwrap().push(
        headers
            .get(header::IF_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );

    let full = state.body();
    let quoted = format!("\"{etag}\"");
    // If-Range honored ONLY on exact match (strong etag semantics).
    let if_range_ok = match headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) {
        Some(v) => v == quoted,
        None => true,
    };
    let start = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split('-').next())
        .and_then(|v| v.parse::<u64>().ok());

    let (status, slice) = match (start, if_range_ok) {
        (Some(start), true) if start < full.len() as u64 => {
            (StatusCode::PARTIAL_CONTENT, &full[start as usize..])
        }
        // No/ignored range or rejected If-Range: full replay.
        _ => (StatusCode::OK, &full[..]),
    };
    let mut resp = (status, slice.to_vec()).into_response();
    let h = resp.headers_mut();
    if status == StatusCode::PARTIAL_CONTENT {
        let start = start.unwrap_or(0);
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{}", full.len() - 1, full.len()))
                .unwrap(),
        );
    } else {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(full.len()));
    }
    h.insert(header::ETAG, HeaderValue::from_str(&quoted).unwrap());
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

async fn spawn_mock(state: ServerState) -> SocketAddr {
    let app = Router::new()
        .route("/file", get(ranged_file))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn temp_sink(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("pg-b36-{name}-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Sink pre-seeded with the FIRST 400 bytes of the v1 body: the
/// single-stream partial a previous session left behind.
fn sink_partial(name: &str, n: usize) -> std::path::PathBuf {
    let p = temp_sink(name);
    let body: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&p, &body[..n]).unwrap();
    p
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

/// Floor above the 1000-byte body → everything routes single-stream.
fn cfg() -> SegmentConfig {
    SegmentConfig::new(4000, 4)
}

/// Engine wired to a fresh in-memory store (B36 both sides).
fn engine() -> (HttpEngine, Store, ServerState) {
    let store = Store::open_memory().unwrap();
    let engine = HttpEngine::new()
        .unwrap()
        .with_validator_store(store.clone());
    (
        engine,
        store,
        ServerState {
            etag: Arc::new(Mutex::new("v1")),
            if_ranges: Arc::default(),
        },
    )
}

/// Recorder progress sink (keeps the signature happy; events unused).
#[derive(Default)]
struct Rec(Mutex<Vec<u64>>);
impl peregrine_api::ProgressSink for Rec {
    fn on_progress(&self, p: &peregrine_api::DownloadProgress) {
        self.0.lock().unwrap().push(p.bytes_done);
    }
}

fn sink_of(p: &std::path::Path) -> std::path::PathBuf {
    p.to_path_buf()
}

// --------------------------------------------------------------------

/// Write side: a fresh single-stream download persists a
/// validator-ONLY row — etag present, total NULL (Route 1 must never
/// mistake it for a segmented partial).
#[tokio::test]
async fn fresh_download_persists_validator_only_row() {
    let (engine, store, state) = engine();
    let addr = spawn_mock(state.clone()).await;
    let sink = temp_sink("fresh");

    engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
            },
            Arc::new(Rec::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    let row = store
        .get_task(&format!("http://{addr}/file"), &sink_of(&sink))
        .await
        .unwrap()
        .expect("row must exist after first response");
    assert_eq!(row.etag.as_deref(), Some("\"v1\""));
    assert!(row.total.is_none(), "single-stream row must be total-NULL");
    assert_eq!(std::fs::read(&sink).unwrap(), state.body());
}

/// Read side happy path: resume with a stored validator sends
/// `If-Range: "v1"`, gets 206, appends — byte-exact file.
#[tokio::test]
async fn resume_with_stored_validator_gets_206_append() {
    let (engine, store, state) = engine();
    let addr = spawn_mock(state.clone()).await;
    let url = format!("http://{addr}/file");
    let sink = sink_partial("resume206", 400);

    // Seed the validator row exactly as the write side would: first
    // response of the PREVIOUS session persisted it.
    store
        .upsert_task(&url, &sink_of(&sink), None, Some("\"v1\""))
        .await
        .unwrap();

    engine
        .download_auto(
            DownloadJob {
                url: url.clone(),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Rec::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // The server saw If-Range: "v1" on the resume request.
    assert!(
        state
            .if_ranges
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.as_deref() == Some("\"v1\"")),
        "resume must send the stored validator as If-Range"
    );
    assert_eq!(std::fs::read(&sink).unwrap(), state.body());
    // And the row still carries the (unchanged) validator.
    let row = store
        .get_task(&url, &sink_of(&sink))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.etag.as_deref(), Some("\"v1\""));
    assert!(row.total.is_none());
}

/// Read side hostile path: the remote changed (v1 → v2). The stored
/// validator goes out as If-Range, the server rejects it with a 200
/// full replay, and the truncate branch rewrites from zero — the file
/// is the NEW body, not a v1/v2 glue.
#[tokio::test]
async fn changed_remote_triggers_full_rewrite() {
    let (engine, store, state) = engine();
    let addr = spawn_mock(state.clone()).await;
    let url = format!("http://{addr}/file");
    let sink = sink_partial("rewrite", 400);

    store
        .upsert_task(&url, &sink_of(&sink), None, Some("\"v1\""))
        .await
        .unwrap();
    // The remote mutates between sessions.
    *state.etag.lock().unwrap() = "v2";

    engine
        .download_auto(
            DownloadJob {
                url: url.clone(),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Rec::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // If-Range WAS sent (and rejected) — the discriminator between
    // "we detected the change" and "the server never let us ask".
    assert!(
        state
            .if_ranges
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.as_deref() == Some("\"v1\"")),
        "resume must send the stale stored validator"
    );
    // THE assertion: byte-exact NEW body — no 400-byte v1 prefix glued
    // onto a v2 tail.
    let expect: Vec<u8> = (0..1000u32).map(|i| ((i + 7) % 251) as u8).collect();
    assert_eq!(std::fs::read(&sink).unwrap(), expect);
    // The row's validator was refreshed to the new one.
    let row = store
        .get_task(&url, &sink_of(&sink))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.etag.as_deref(), Some("\"v2\""));
}

/// No store wired (plain engine, e.g. library users / tests): no
/// write side, no crash — validator-less resume, exactly today's
/// behavior.
#[tokio::test]
async fn engine_without_store_degrades_silently() {
    let state = ServerState {
        etag: Arc::new(Mutex::new("v1")),
        if_ranges: Arc::default(),
    };
    let addr = spawn_mock(state.clone()).await;
    let sink = sink_partial("nostore", 400);
    let store = Store::open_memory().unwrap();
    let engine = HttpEngine::new().unwrap(); // no validator store

    engine
        .download_auto(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: None,
            },
            &cfg(),
            &store,
            Arc::new(Rec::default()),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // No row was created by the engine (Route 2 read side also found
    // nothing) — and the file is still correct via plain Range.
    assert_eq!(std::fs::read(&sink).unwrap(), state.body());
    let rows = state.if_ranges.lock().unwrap();
    assert!(
        rows.iter().all(|v| v.is_none()),
        "no If-Range without a store"
    );
}
