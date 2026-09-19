//! `download()` behaviour against a local axum mock: streaming, resume
//! (206/If-Range/200-replay), misalignment refusal, short-read detection,
//! unknown-size EOF, redirect following, and drop-cancellation leaving a
//! valid partial prefix.

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::stream::StreamExt as _;
use peregrine_api::{
    ApiError, DownloadJob, DownloadProgress, IfRangeValidator, NoProgress, ProgressSink,
    ProtocolEngine, ResumeContext,
};
use peregrine_engine_http::HttpEngine;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// The canonical body every handler serves: 1000 deterministic bytes.
fn body_bytes() -> Vec<u8> {
    (0..1000u32).map(|i| (i % 251) as u8).collect()
}

/// Full-fidelity Range/If-Range handler over `body_bytes()`:
/// - Range → 206 with correct Content-Range (or 416 when unsatisfiable)
/// - If-Range present and NOT matching `v1` → 200 full replay (mutated)
async fn ranged_file(headers: HeaderMap) -> Response {
    serve_ranged(&headers, &body_bytes(), "v1")
}

/// `etag` is the BARE validator ("v1"); wire format adds quotes.
fn serve_ranged(req_headers: &HeaderMap, full: &[u8], etag: &str) -> Response {
    let quoted = format!("\"{etag}\"");
    let if_range_ok = match req_headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) => v == quoted || v.contains("Mon, 07 Sep 2026"),
        None => true, // no validator → ranges are trusted
    };
    let range = req_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split('-').next())
        .and_then(|v| v.parse::<u64>().ok());

    match (range, if_range_ok) {
        (Some(start), true) => {
            if start >= full.len() as u64 {
                // Canonical 416: the server's authoritative total
                // (`bytes */T`), as nginx/S3 send it.
                let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "out of range").into_response();
                resp.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", full.len())).unwrap(),
                );
                return resp;
            }
            let slice = &full[start as usize..];
            let mut resp = (StatusCode::PARTIAL_CONTENT, slice.to_vec()).into_response();
            let h = resp.headers_mut();
            h.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!(
                    "bytes {start}-{}/{}",
                    full.len() as u64 - 1,
                    full.len()
                ))
                .unwrap(),
            );
            h.insert(header::ETAG, HeaderValue::from_str(&quoted).unwrap());
            h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            resp
        }
        // Range ignored (or resource mutated under If-Range): full replay.
        _ => {
            let mut resp = (StatusCode::OK, full.to_vec()).into_response();
            resp.headers_mut().insert(
                header::ETAG,
                HeaderValue::from_str(&format!("\"{etag}\"")).unwrap(),
            );
            resp
        }
    }
}

/// Lies about the total: a clean chunked 200 of 500 bytes, while the
/// caller's probe said 1000. The protocol layer sees a perfectly normal
/// stream — only OUR total-vs-bytes check can catch the shortage.
async fn short_body(headers: HeaderMap) -> Response {
    use axum::body::Bytes;
    let half = body_bytes()[..500].to_vec();
    let stream = futures::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(half))]);
    let resp = (StatusCode::OK, axum::body::Body::from_stream(stream)).into_response();
    // The ranged variant (206 + generous Content-Range total) exercises
    // the same check on the resume path.
    if headers.contains_key(header::RANGE) {
        let mut resp = resp;
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-999/1000"),
        );
        return resp;
    }
    resp
}

/// Chunked (no Content-Length): EOF is the only end signal.
async fn chunked_file() -> Response {
    use axum::body::Bytes;
    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from_static(&[1u8; 300])),
        Ok(Bytes::from_static(&[2u8; 300])),
    ];
    let stream = futures::stream::iter(chunks);
    (StatusCode::OK, axum::body::Body::from_stream(stream)).into_response()
}

async fn redirect_to_file() -> Response {
    ([(header::LOCATION, "/file")], StatusCode::FOUND).into_response()
}

async fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// One byte every 40ms — for cancellation tests.
async fn drip() -> Response {
    use axum::body::Bytes;
    let chunks: VecDeque<Result<Bytes, std::io::Error>> =
        (0..250).map(|_| Ok(Bytes::from(vec![7u8; 64]))).collect();
    let stream = futures::stream::iter(chunks).then(|c| async move {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        c
    });
    (StatusCode::OK, axum::body::Body::from_stream(stream)).into_response()
}

async fn spawn_mock() -> SocketAddr {
    let app = Router::new()
        .route("/file", get(ranged_file))
        .route("/short", get(short_body))
        .route("/chunked", get(chunked_file))
        .route("/redir", get(redirect_to_file))
        .route("/nocontent", get(no_content))
        .route("/drip", get(drip));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn temp_sink(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("peregrine-dl-test-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_file(&p);
    p
}

/// Collects every progress event for inspection.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<DownloadProgress>>,
}

impl ProgressSink for Recorder {
    fn on_progress(&self, p: &DownloadProgress) {
        self.events.lock().unwrap().push(*p);
    }
}

fn recorder() -> Arc<Recorder> {
    Arc::new(Recorder::default())
}

// --------------------------------------------------------------------

#[tokio::test]
async fn fresh_download_streams_all_bytes() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("fresh");

    let rec = recorder();
    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: None,
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            rec.clone(),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.bytes_written, 1000);
    assert_eq!(out.total_bytes, Some(1000));
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());

    // Progress is monotone and ends at the total.
    let evts = rec.events.lock().unwrap().clone();
    assert!(!evts.is_empty());
    assert_eq!(evts.last().unwrap().bytes_done, 1000);
    for w in evts.windows(2) {
        assert!(
            w[0].bytes_done <= w[1].bytes_done,
            "progress must be monotone"
        );
    }
}

#[tokio::test]
async fn resume_appends_from_offset() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("resume");

    // Pre-existing partial: first 400 bytes.
    std::fs::write(&sink, &body_bytes()[..400]).unwrap();

    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: Some(IfRangeValidator::StrongEtag("\"v1\"".into())),
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.bytes_written, 600, "this session wrote the remainder");
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    // The outcome carries the CURRENT validator, quoted as served —
    // what M2 persists for the next resume.
    assert_eq!(
        out.final_validator,
        Some(IfRangeValidator::StrongEtag("\"v1\"".into()))
    );
}

#[tokio::test]
async fn short_206_on_resume_is_an_error_not_a_completion() {
    // Honors Range but sends only 100 of the promised 600 bytes —
    // a well-formed 206 whose body ends early (flaky origin, truncated
    // proxy buffer). Must surface as Err, never as completed=false Ok.
    async fn truncated_206(headers: HeaderMap) -> Response {
        let start = headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split('-').next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let short = &body_bytes()[start as usize..start as usize + 100];
        let mut resp = (StatusCode::PARTIAL_CONTENT, short.to_vec()).into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 400-999/1000"),
        );
        resp
    }
    let app = Router::new().route("/trunc206", get(truncated_206));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("short206");
    std::fs::write(&sink, &body_bytes()[..400]).unwrap();

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/trunc206"),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("short read"), "got: {err}");
    // The fetched 100 bytes still landed — longer partial, resumable.
    assert_eq!(std::fs::read(&sink).unwrap().len(), 500);
}

#[tokio::test]
async fn four16_without_a_settling_total_is_an_error() {
    // 416 whose `Content-Range: bytes */T` DISAGREES with the resume
    // offset: not "already complete", and guessing would risk a false
    // completion — the worst failure mode. Must surface as Err.
    async fn liar416() -> Response {
        let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "no").into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes */777"),
        );
        resp
    }
    let app = Router::new().route("/liar416", get(liar416));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("liar416");
    std::fs::write(&sink, &body_bytes()[..400]).unwrap();

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/liar416"),
                sink,
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("416"), "got: {msg}");
    // Since the QA-E2E Bug 3 self-heal this error is reached only
    // AFTER one bounded from-zero retry: the server 416s the retried
    // (Range-less) request as well, and the `healed` flag stops the
    // recursion. The assertion above therefore pins BOTH hops.
}

#[tokio::test]
async fn stale_offset_416_self_heals_to_a_full_rewrite() {
    // QA-E2E Bug 3: canonical repro — the resume offset lies about
    // the sink (sparse-preallocated file whose segment rows were
    // purged, so len == total fed a fresh single-stream re-add). A
    // 416 that does NOT settle as "already complete" must trigger
    // ONE bounded restart from zero: the retry omits Range, gets the
    // full 200, truncates the stale sink and rewrites it verbatim.
    async fn cond416(headers: HeaderMap) -> Response {
        let has_range = headers.contains_key(header::RANGE);
        if has_range {
            // No Content-Range, and the body is an error page: the
            // mirror-style 416 QA hit in the wild (Yandex).
            (StatusCode::RANGE_NOT_SATISFIABLE, "out of range").into_response()
        } else {
            (StatusCode::OK, body_bytes()).into_response()
        }
    }
    let app = Router::new().route("/cond416", get(cond416));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("cond416");
    // The lying sink: 400 bytes of stale content at the resume offset.
    std::fs::write(&sink, &body_bytes()[..400]).unwrap();

    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/cond416"),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed, "self-healed session must complete");
    assert_eq!(
        std::fs::read(&sink).unwrap(),
        body_bytes(),
        "stale prefix must be fully rewritten, never glued"
    );
    assert_eq!(
        out.bytes_written, 1000,
        "the healed session wrote the whole body"
    );
}

#[tokio::test]
async fn mutated_resource_replays_full_body_over_partial() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("mutated");

    // Partial from the OLD resource; If-Range validator no longer matches.
    std::fs::write(&sink, b"stale- prefix that must disappear").unwrap();

    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 35,
                    validator: Some(IfRangeValidator::StrongEtag("\"different\"".into())),
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    // 200 replay: truncate + rewrite, never glue.
    assert!(out.completed);
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
    // After a replay the OLD validator is stale; the outcome carries
    // the fresh one ("v1" now) for the next resume.
    assert_eq!(
        out.final_validator,
        Some(IfRangeValidator::StrongEtag("\"v1\"".into()))
    );
}

#[tokio::test]
async fn etag_flipped_206_is_refused_not_glued() {
    // A server that ignores If-Range: honors Range, serves CURRENT
    // bytes with a NEW etag. Gluing would corrupt the file silently —
    // the engine must refuse on the validator mismatch.
    async fn flip_etag_206(headers: HeaderMap) -> Response {
        let start = headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split('-').next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let slice = &body_bytes()[start as usize..];
        let mut resp = (StatusCode::PARTIAL_CONTENT, slice.to_vec()).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {start}-{}/{}",
                body_bytes().len() - 1,
                body_bytes().len()
            ))
            .unwrap(),
        );
        h.insert(header::ETAG, HeaderValue::from_static("\"v9\""));
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        resp
    }
    let app = Router::new().route("/flipetag", get(flip_etag_206));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("flipetag");
    std::fs::write(&sink, &body_bytes()[..400]).unwrap();

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/flipetag"),
                sink: sink.clone(),
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: Some(IfRangeValidator::StrongEtag("\"v1\"".into())),
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("changed mid-resume"), "got: {msg}");
    // And crucially nothing was written.
    assert_eq!(std::fs::read(&sink).unwrap().len(), 400);
}

#[tokio::test]
async fn misaligned_206_is_refused_not_glued() {
    // A handler that answers 206 from byte 0 no matter what we asked.
    async fn liar() -> Response {
        let full = body_bytes();
        let mut resp = (StatusCode::PARTIAL_CONTENT, full).into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-999/1000"),
        );
        resp
    }
    let app = Router::new().route("/liar", get(liar));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("misaligned");
    std::fs::write(&sink, &body_bytes()[..400]).unwrap();

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/liar"),
                sink,
                resume: Some(ResumeContext {
                    start_offset: 400,
                    validator: None,
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("byte 0, expected 400"), "got: {msg}");
}

#[tokio::test]
async fn short_read_is_an_error_not_a_completion() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("short");

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/short"),
                sink: sink.clone(),
                resume: None,
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("short read"), "got: {err}");
    // The partial prefix stays on disk, resumable.
    assert_eq!(std::fs::read(&sink).unwrap().len(), 500);
}

#[tokio::test]
async fn unknown_size_completes_at_eof() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("chunked");

    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/chunked"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed, "unknown size: EOF is completion");
    assert_eq!(out.bytes_written, 600);
    assert_eq!(std::fs::read(&sink).unwrap().len(), 600);
}

#[tokio::test]
async fn already_complete_resume_short_circuits_on_416() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("done416");
    std::fs::write(&sink, body_bytes()).unwrap();

    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink,
                resume: Some(ResumeContext {
                    start_offset: 1000,
                    validator: Some(IfRangeValidator::StrongEtag("\"v1\"".into())),
                }),
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.bytes_written, 0, "nothing to fetch");
}

#[tokio::test]
async fn download_follows_redirects() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("redir");

    let out = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/redir"),
                sink: sink.clone(),
                resume: None,
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert!(out.final_url.ends_with("/file"));
    assert_eq!(std::fs::read(&sink).unwrap(), body_bytes());
}

#[tokio::test]
async fn bodyless_success_status_is_refused() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/nocontent"),
                sink: temp_sink("nocontent"),
                resume: None,
                expected_total: None,
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unexpected status 204"));
}

#[tokio::test]
async fn dropping_the_future_leaves_a_valid_prefix() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("cancelled");

    // Drip feeds 64B/40ms; cancel after ~5 chunks (~320B) landed.
    // Scoped so the pinned future DROPS before we inspect the sink —
    // kill -9 semantics: abandoning the future keeps the on-disk prefix.
    let on_disk = {
        let fut = engine.download(
            DownloadJob {
                url: format!("http://{addr}/drip"),
                sink: sink.clone(),
                resume: None,
                expected_total: Some(16000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            Arc::new(NoProgress),
            token(),
            &peregrine_api::budget::BudgetChain::unlimited(),
        );
        tokio::pin!(fut);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(300), fut.as_mut()).await;
        // `fut` dropped here at block end — the cancellation path
        // under test, not just scope-exit bookkeeping.
        std::fs::read(&sink).unwrap()
    };
    assert!(
        !on_disk.is_empty() && on_disk.len().is_multiple_of(64),
        "partial file must be a whole-chunk prefix, got {}",
        on_disk.len()
    );
    assert!(on_disk.iter().all(|&b| b == 7));
}

/// A live (never-cancelled) token for tests that don't exercise cancellation.
fn token() -> CancellationToken {
    CancellationToken::new()
}

// --------------------------------------------------------------------
// M2-b: cooperative cancellation (CancellationToken)

/// Pause mid-download: the engine must stop reading, flush, and
/// return Cancelled — with a valid partial on disk that a later
/// resume continues from (not restarts).
#[tokio::test]
async fn cancel_mid_stream_leaves_valid_partial() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("cancel-mid");

    let token = CancellationToken::new();
    let t2 = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        t2.cancel();
    });

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/drip"),
                sink: sink.clone(),
                resume: None,
                expected_total: Some(16000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            recorder(),
            token,
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ApiError::Cancelled), "got {err:?}");
    // A partial exists, is a prefix the next resume can build on
    // (drip chunks are 64B frames), and is strictly short of the end.
    let len = std::fs::metadata(&sink).unwrap().len();
    assert!(len > 0, "cancelled download must leave its partial");
    assert_eq!(len % 64, 0);
    assert!(len < 16000);
    // Full resume-after-cancel round-trip lives in the segment suite
    // (cancel_mid_swarm_keeps_cursors_and_resumes_cleanly); here the
    // on-disk prefix is the contract.
}

/// A token already cancelled at call time returns Cancelled without
/// touching the network.
#[tokio::test]
async fn cancel_before_start_returns_immediately() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();
    let sink = temp_sink("cancel-upfront");

    let token = CancellationToken::new();
    token.cancel();

    let err = engine
        .download(
            DownloadJob {
                url: format!("http://{addr}/file"),
                sink: sink.clone(),
                resume: None,
                expected_total: Some(1000),
                mirrors: Vec::new(),
                fetch_base: None,
            },
            recorder(),
            token,
            &peregrine_api::budget::BudgetChain::unlimited(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Cancelled), "got {err:?}");
    // Nothing written: the file may exist (pre-open) but is empty.
    assert_eq!(std::fs::metadata(&sink).map(|m| m.len()).unwrap_or(0), 0);
}
