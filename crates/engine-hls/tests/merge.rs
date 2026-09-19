//! Full-pipeline integration: a local axum origin serves master +
//! variant media playlists, plaintext and AES-128 segments, an
//! fMP4 init segment and a key endpoint. `download_merge` runs
//! against it and we assert the MERGED file byte-for-byte.

use peregrine_api::budget::{BudgetChain, RateBudget};
use peregrine_api::download::{DownloadJob, NoProgress};
use peregrine_engine_hls::HlsEngine;
use std::sync::Arc;
use tempfile::TempDir;

/// Build a small VOD origin and return (base_url, expected_merged).
async fn origin() -> (String, Vec<u8>) {
    use axum::Router;
    use axum::routing::get;

    // Segments: 3 x 40 bytes deterministic plaintext.
    let segs: Arc<Vec<Vec<u8>>> = Arc::new(
        (0..3u16)
            .map(|i| (0u8..40).map(move |j| (i * 40 + j as u16) as u8).collect())
            .collect::<Vec<_>>(),
    );
    let init: Vec<u8> = (200u8..216).collect();

    let app = Router::new()
        .route(
            "/v/master.m3u8",
            get(|| async {
                "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1000000
lo/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4000000
hi/index.m3u8
"
            }),
        )
        .route(
            "/v/hi/index.m3u8",
            get(|| async {
                "#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MAP:URI=\"init.mp4\"
#EXT-X-KEY:METHOD=AES-128,URI=\"keys/k1\"
#EXTINF:4.0,
seg0.ts
#EXTINF:4.0,
seg1.ts
#EXT-X-KEY:METHOD=NONE
#EXTINF:4.0,
seg2.ts
#EXT-X-ENDLIST
"
            }),
        )
        .route(
            "/v/hi/init.mp4",
            get(|| async { (200u8..216).collect::<Vec<u8>>() }),
        )
        .route("/v/hi/keys/k1", get(|| async { vec![42u8; 16] }))
        .route(
            "/v/hi/seg0.ts",
            get({
                let segs = segs.clone();
                move || {
                    let plain = segs[0].clone();
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                        encrypt(&plain, &[42u8; 16], &iv(0))
                    }
                }
            }),
        )
        .route(
            "/v/hi/seg1.ts",
            get({
                let segs = segs.clone();
                move || {
                    let plain = segs[1].clone();
                    async move { encrypt(&plain, &[42u8; 16], &iv(1)) }
                }
            }),
        )
        .route(
            "/v/hi/seg2.ts",
            get({
                let segs = segs.clone();
                move || {
                    let plain = segs[2].clone();
                    async move { plain }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let mut expected = init.clone();
    for s in segs.iter() {
        expected.extend_from_slice(s);
    }
    (format!("http://{addr}/v/master.m3u8"), expected)
}

fn iv(seq: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[8..].copy_from_slice(&seq.to_be_bytes());
    b
}

fn encrypt(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv)
        .unwrap()
        .encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(plain)
}

fn unlimited() -> BudgetChain {
    BudgetChain {
        local: RateBudget::unlimited(),
        global: RateBudget::unlimited(),
    }
}

fn job(url: &str, sink: &std::path::Path) -> DownloadJob {
    DownloadJob {
        url: url.to_string(),
        sink: sink.to_path_buf(),
        resume: None,
        expected_total: None,
        mirrors: Vec::new(),
        fetch_base: None,
    }
}

/// Workspace feature unification links two rustls providers (see
/// scheduler::tls); install ours before any TLS consumer runs.
fn init_tls() {
    peregrine_scheduler::tls::init_tls();
}

#[tokio::test]
async fn merges_master_aes_and_map_end_to_end() {
    init_tls();
    let (base, expected) = origin().await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("out.ts");

    let engine = HlsEngine::new().unwrap();
    let budget = unlimited();
    let out = engine
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &budget,
        )
        .await
        .unwrap();

    assert!(out.completed);
    assert_eq!(out.bytes_written, expected.len() as u64);
    assert_eq!(out.final_url, base.replace("master.m3u8", "hi/index.m3u8"));
    let on_disk = std::fs::read(&sink).unwrap();
    assert_eq!(on_disk, expected, "merged bytes must match init+segs");
    // parts dir removed after success (R2 P1-4: compute the path
    // the SAME way parts_dir does — append .parts to the full name).
    let parts = {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".parts");
        std::path::PathBuf::from(s)
    };
    assert!(!parts.exists(), "parts dir must be removed after merge");
}

#[tokio::test]
async fn cancel_midway_leaves_resumable_parts_and_no_tmp() {
    init_tls();
    let (base, expected) = origin().await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("out.ts");

    let engine = HlsEngine::new().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let c2 = cancel.clone();
    // Cancel after the first progress frame (playlist resolved,
    // first bytes flowing): deterministic enough for a 3-seg file.
    let sink_progress: peregrine_api::download::SharedProgressSink = Arc::new(CancelAfter {
        cancel: c2,
        seen: std::sync::atomic::AtomicU32::new(0),
    });
    let budget = unlimited();
    let res = engine
        .download_merge(&job(&base, &sink), sink_progress, cancel, &budget)
        .await;
    assert!(res.is_err(), "cancelled run must not report success");

    // No *.tmp may survive (cancel path removes them).
    let parts = {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".parts");
        std::path::PathBuf::from(s)
    };
    if parts.exists() {
        for e in std::fs::read_dir(&parts).unwrap().flatten() {
            assert!(
                !e.file_name().to_string_lossy().ends_with(".tmp"),
                "tmp leaked: {e:?}"
            );
        }
    }

    // Resume run completes with identical bytes.
    let engine2 = HlsEngine::new().unwrap();
    engine2
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&sink).unwrap(), expected);
}

struct CancelAfter {
    cancel: tokio_util::sync::CancellationToken,
    seen: std::sync::atomic::AtomicU32,
}

impl peregrine_api::download::ProgressSink for CancelAfter {
    fn on_progress(&self, _p: &peregrine_api::download::DownloadProgress) {
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1 {
            self.cancel.cancel();
        }
    }
}

// ---- M4-b1: live (no ENDLIST) recording ----

/// Deterministic sliding-window live origin.
///
/// Window = 3 seqs; each playlist GET (after the first) slides the
/// window one seq forward. After `end_after` polls the playlist
/// carries ENDLIST (normal event end). `skip_to` jumps the window
/// start on poll #2 (gap scenario). Segments are 40 deterministic
/// bytes each: seq n → [n*8, n*8+40).
#[derive(Clone)]
struct LiveSpec {
    end_after: usize,
    skip_to: Option<u64>,
    /// Never advance the window (stalled-stream scenario).
    frozen: bool,
    /// `#EXT-X-TARGETDURATION` value — drives the engine's stall
    /// budget and (absent an override) poll cadence.
    td: f64,
    /// Advance the window every Nth reload (1 = every reload).
    /// `advance_every: 3` models a td=60 slow stream under a fast
    /// test cadence: two empty polls between real segment arrivals.
    advance_every: usize,
    /// Serve HTTP 500 for every playlist reload from this poll
    /// number on (join excluded — it must succeed). 0 = never.
    fail_from: usize,
    /// Artificial per-segment response delay (cancel tests).
    seg_delay: std::time::Duration,
    /// Serve AES-128 ciphertext with `#EXT-X-KEY` (no explicit IV →
    /// seq-derived IVs, as RFC 8216 §5.2.1.1 defaults).
    aes: bool,
}

fn live_default() -> LiveSpec {
    LiveSpec {
        end_after: usize::MAX,
        skip_to: None,
        frozen: false,
        td: 4.0,
        advance_every: 1,
        fail_from: 0,
        seg_delay: std::time::Duration::ZERO,
        aes: false,
    }
}

/// AES-128 key served by `live_origin` when `spec.aes`.
const LIVE_KEY: [u8; 16] = [42u8; 16];

async fn live_origin(spec: LiveSpec) -> String {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Live {
        window_start: u64,
        polls: usize,
        ended: bool,
    }
    let live = Arc::new(Mutex::new(Live {
        window_start: 0,
        polls: 0,
        ended: false,
    }));

    let app = Router::new()
        .route(
            "/live.m3u8",
            get({
                let live = live.clone();
                let s = spec.clone();
                move || {
                    let live = live.clone();
                    let s = s.clone();
                    async move {
                        let mut st = live.lock().unwrap();
                        st.polls += 1;
                        if s.fail_from > 0 && st.polls > 1 && st.polls >= s.fail_from {
                            drop(st);
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "cdn hard-down".to_string(),
                            );
                        }
                        let advance = !s.frozen
                            && st.polls > 1
                            && (st.polls - 1).is_multiple_of(s.advance_every.max(1));
                        if advance {
                            st.window_start += 1;
                            if st.polls == 2
                                && let Some(to) = s.skip_to
                            {
                                st.window_start = to;
                            }
                        }
                        if st.polls >= s.end_after {
                            st.ended = true;
                        }
                        let start = st.window_start;
                        let ended = st.ended;
                        drop(st);
                        // RFC 8216 §6.2.2: a sliding window MUST raise
                        // MEDIA-SEQUENCE — segment identity is
                        // media_sequence + index, so without this the
                        // engine is CORRECT to treat every reload as
                        // the same seqs (first bug found by these
                        // tests was the mock, not the engine).
                        let mut body = format!(
                            "#EXTM3U\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{start}\n",
                            s.td
                        );
                        if s.aes {
                            body.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n");
                        }
                        for seq in start..start + 3 {
                            body.push_str(&format!("#EXTINF:{},\nseg/{seq}.ts\n", s.td));
                        }
                        if ended {
                            body.push_str("#EXT-X-ENDLIST\n");
                        }
                        (StatusCode::OK, body)
                    }
                }
            }),
        )
        .route("/key.bin", get(|| async move { LIVE_KEY.to_vec() }))
        .route(
            "/seg/{n}",
            get({
                let s2 = spec.clone();
                move |axum::extract::Path(n): axum::extract::Path<String>| async move {
                    let n: u64 = n.trim_end_matches(".ts").parse().unwrap();
                    tokio::time::sleep(s2.seg_delay).await;
                    if s2.aes {
                        encrypt(&seg_bytes(n), &LIVE_KEY, &iv(n))
                    } else {
                        seg_bytes(n)
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/live.m3u8")
}

fn seg_bytes(n: u64) -> Vec<u8> {
    (n * 8..n * 8 + 40).map(|b| b as u8).collect()
}

/// Normal event: window slides, ENDLIST lands on poll 4 → recorded
/// = every seq from join (0) to end (5), merged in order, parts dir
/// cleaned up.
#[tokio::test]
async fn live_stream_recorded_until_endlist() {
    init_tls();
    let base = live_origin(LiveSpec {
        end_after: 4,
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("live.ts");

    let engine = HlsEngine::new()
        .unwrap()
        .with_concurrency(2)
        .with_poll_cadence_secs(0.10);
    let out = engine
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    let mut expected = Vec::new();
    for n in 0..=5u64 {
        expected.extend_from_slice(&seg_bytes(n));
    }
    assert_eq!(out.bytes_written, expected.len() as u64);
    assert_eq!(std::fs::read(&sink).unwrap(), expected);
    let parts = {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".parts");
        std::path::PathBuf::from(s)
    };
    assert!(!parts.exists(), "live merge must clean parts dir");
}

/// Frozen upstream (no new segments, no ENDLIST): fail loudly
/// once the TIME budget (3 × td=1s → 3s) runs out, instead of
/// polling forever. With the pre-M4-b1.1 fixed poll count this
/// fired after 6 polls regardless of td.
#[tokio::test]
async fn live_stall_fails_loudly() {
    init_tls();
    let base = live_origin(LiveSpec {
        frozen: true,
        td: 1.0,
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("stall.ts");

    let started = std::time::Instant::now();
    let err = HlsEngine::new()
        .unwrap()
        .with_poll_cadence_secs(0.05)
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stalled"), "{err}");
    // Budget = 3 × td(1s) = 3s; allow slack, but it must be
    // prompt — not the pre-fix 48s worst case, never forever.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "stall detection must be prompt, took {:?}",
        started.elapsed()
    );
}

/// Window jumps past seqs we never saw (skip_to=4 on poll 2): the
/// parts land but the MERGE catches the hole and fails instead of
/// emitting a corrupt file.
#[tokio::test]
async fn live_gap_from_window_slide_detected() {
    init_tls();
    let base = live_origin(LiveSpec {
        end_after: 5,
        skip_to: Some(4),
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("gap.ts");

    let err = HlsEngine::new()
        .unwrap()
        .with_poll_cadence_secs(0.05)
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("segment gap"), "{err}");
    // No half-merged output may exist.
    assert!(!sink.exists(), "gap run must not emit an output file");
}

/// Cancel while polling mid-recording: prompt Cancelled, no *.tmp,
/// already-recorded parts stay for resume.
#[tokio::test]
async fn live_cancel_stops_promptly() {
    init_tls();
    let base = live_origin(live_default()).await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("cancel.ts");

    let cancel = tokio_util::sync::CancellationToken::new();
    let engine = HlsEngine::new().unwrap().with_poll_cadence_secs(0.30);
    let j = job(&base, &sink);
    let handle = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            engine
                .download_merge(&j, Arc::new(NoProgress), cancel, &unlimited())
                .await
        }
    });
    // Let it join + fetch the first window, then cancel between polls.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let started = std::time::Instant::now();
    cancel.cancel();
    let res = handle.await.unwrap();
    assert!(
        matches!(res, Err(ref e) if fetch_is_cancel(e)),
        "got {res:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "cancel must return within one poll interval"
    );
    let parts = {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".parts");
        std::path::PathBuf::from(s)
    };
    assert!(parts.exists(), "recorded parts must survive cancel");
    for e in std::fs::read_dir(&parts).unwrap().flatten() {
        assert!(!e.file_name().to_string_lossy().ends_with(".tmp"));
    }
}

fn fetch_is_cancel(e: &peregrine_engine_hls::HlsError) -> bool {
    e.to_string().contains("cancelled")
}

/// A td=60 stream (radio/camera cadence) that only advances its
/// window every 3rd reload must NOT be stall-killed: the budget is
/// time-based (3 × 60s), not a fixed empty-poll count. Regression
/// guard for R2 P1-2 — the old 6-poll counter killed it at 48s.
#[tokio::test]
async fn live_slow_stream_td60_not_stall_killed() {
    init_tls();
    let base = live_origin(LiveSpec {
        td: 60.0,
        advance_every: 3,
        end_after: 12, // window: 0,0,0,1,1,1,2,2,2,3,3,3 then ENDLIST
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("slow.ts");

    let engine = HlsEngine::new()
        .unwrap()
        .with_concurrency(2)
        // Test cadence (NOT td/2=30s): two empty polls between
        // arrivals would trip a count-based stall detector.
        .with_poll_cadence_secs(0.05);
    let out = engine
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();

    assert!(out.completed);
    let mut expected = Vec::new();
    for n in 0..=5u64 {
        expected.extend_from_slice(&seg_bytes(n));
    }
    assert_eq!(std::fs::read(&sink).unwrap(), expected);
}

/// Poll failures: 2 transient 500s are absorbed (existing test);
/// 3 CONSECUTIVE 500s fail the task loudly instead of polling a
/// dead CDN forever.
#[tokio::test]
async fn live_poll_failures_three_consecutive_fail_loudly() {
    init_tls();
    let base = live_origin(LiveSpec {
        fail_from: 2, // every reload 500s; join (poll 1) still OK
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("dead-cdn.ts");

    let started = std::time::Instant::now();
    let err = HlsEngine::new()
        .unwrap()
        .with_poll_cadence_secs(0.05)
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("http 500"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

/// Cancel WHILE A SEGMENT DOWNLOAD IS IN FLIGHT (not in the poll
/// gap — that is `live_cancel_stops_promptly`): the per-segment
/// select must return cancelled immediately even though the origin
/// still owes us 5s of bytes. No `.tmp` debris may remain.
#[tokio::test]
async fn live_cancel_during_segment_download() {
    init_tls();
    let base = live_origin(LiveSpec {
        seg_delay: std::time::Duration::from_secs(5),
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("cancel-mid-seg.ts");

    let cancel = tokio_util::sync::CancellationToken::new();
    let engine = HlsEngine::new().unwrap().with_poll_cadence_secs(0.30);
    let j = job(&base, &sink);
    let handle = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            engine
                .download_merge(&j, Arc::new(NoProgress), cancel, &unlimited())
                .await
        }
    });
    // Join fetch lands the first window; its 3 segments each sleep
    // 5s server-side — cancel lands squarely inside them.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let started = std::time::Instant::now();
    cancel.cancel();
    let res = handle.await.unwrap();
    assert!(
        matches!(res, Err(ref e) if fetch_is_cancel(e)),
        "got {res:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "cancel during segment I/O must not wait out the 5s origin delay"
    );
    let parts = {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".parts");
        std::path::PathBuf::from(s)
    };
    assert!(parts.exists());
    for e in std::fs::read_dir(&parts).unwrap().flatten() {
        assert!(!e.file_name().to_string_lossy().ends_with(".tmp"));
    }
}

/// AES-128 live recording end-to-end: key fetched at join, parts
/// stored ENCRYPTED (plaintext never persists), decrypted in
/// memory at merge. No explicit IV → seq-derived IVs.
#[tokio::test]
async fn live_aes128_roundtrip() {
    init_tls();
    let base = live_origin(LiveSpec {
        end_after: 4,
        aes: true,
        ..live_default()
    })
    .await;
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("enc-live.ts");

    let engine = HlsEngine::new().unwrap().with_poll_cadence_secs(0.05);
    let out = engine
        .download_merge(
            &job(&base, &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    assert!(out.completed);
    let mut expected = Vec::new();
    for n in 0..=5u64 {
        expected.extend_from_slice(&seg_bytes(n));
    }
    assert_eq!(std::fs::read(&sink).unwrap(), expected);
}

/// A CDN blip: playlist GETs #2 and #3 answer 503. The recorder
/// must keep going and still produce the full merged file (P1:
/// single poll failures must not kill a live recording).
#[tokio::test]
async fn live_survives_transient_playlist_failures() {
    init_tls();
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use std::sync::Mutex;

    struct St {
        polls: usize,
        fail_at: Vec<usize>,
    }
    let st = Arc::new(Mutex::new(St {
        polls: 0,
        fail_at: vec![2, 3],
    }));

    let app = Router::new()
        .route(
            "/live.m3u8",
            get({
                let st = st.clone();
                move || {
                    let st = st.clone();
                    async move {
                        let mut s = st.lock().unwrap();
                        s.polls += 1;
                        if s.fail_at.contains(&s.polls) {
                            return (StatusCode::SERVICE_UNAVAILABLE, "cdn blip".to_string());
                        }
                        // poll1: 0..2; poll4: 1..3; poll5: 2..4+ENDLIST
                        let start = if s.polls >= 5 {
                            2
                        } else if s.polls == 4 {
                            1
                        } else {
                            0
                        };
                        let ended = s.polls >= 5;
                        let mut body = format!(
                            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:{start}\n"
                        );
                        let end = start + 3;
                        for seg in start..end {
                            body.push_str(&format!("#EXTINF:4.0,\nseg/{seg}.ts\n"));
                        }
                        if ended {
                            body.push_str("#EXT-X-ENDLIST\n");
                        }
                        (StatusCode::OK, body)
                    }
                }
            }),
        )
        .route(
            "/seg/{n}",
            get(
                |axum::extract::Path(n): axum::extract::Path<String>| async move {
                    let n: u64 = n.trim_end_matches(".ts").parse().unwrap();
                    (n * 8..n * 8 + 40).map(|b| b as u8).collect::<Vec<u8>>()
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("blip.ts");
    let out = HlsEngine::new()
        .unwrap()
        .with_poll_cadence_secs(0.05)
        .download_merge(
            &job(&format!("http://{addr}/live.m3u8"), &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    // recorded = everything seen: poll1 {0,1,2}, poll4 {3}, poll5 {4}
    let mut expected = Vec::new();
    for n in 0..=4u64 {
        expected.extend_from_slice(&seg_bytes(n));
    }
    assert_eq!(out.bytes_written, expected.len() as u64);
    assert_eq!(std::fs::read(&sink).unwrap(), expected);
}

#[tokio::test]
async fn probe_validates_m3u8_and_names_output() {
    init_tls();
    use peregrine_api::engine::ProtocolEngine;
    let (base, _) = origin().await;
    let info = HlsEngine::new().unwrap().probe(&base).await.unwrap();
    assert_eq!(info.filename.as_deref(), Some("master.ts"));
    assert!(info.content_length.is_none());
}

// ---- R2 hardening tests (coverage gaps) ----

/// A server that IGNORES Range (answers 200 with the whole body)
/// must fail the part fetch, not corrupt the merge (fetch.rs 206
/// enforcement).
#[tokio::test]
async fn range_ignoring_server_is_rejected() {
    init_tls();
    use axum::Router;
    use axum::routing::get;
    let app = Router::new()
        .route(
            "/v.m3u8",
            get(|| async {
                "#EXTM3U\n#EXTINF:1.0,\n#EXT-X-BYTERANGE:4@0\nf.bin\n#EXTINF:1.0,\n#EXT-X-BYTERANGE:4\nf.bin\n#EXT-X-ENDLIST\n"
            }),
        )
        .route("/f.bin", get(|| async { vec![1u8, 2, 3, 4, 5, 6, 7, 8] }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("r.bin");
    let base = format!("http://{addr}/v.m3u8");
    let err = HlsEngine::new()
        .unwrap()
        .download_merge(
            &DownloadJob {
                url: base,
                sink,
                resume: None,
                expected_total: None,
                mirrors: Vec::new(),
                fetch_base: None,
            },
            std::sync::Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ignored Range"), "{err}");
}

/// Signed-CDN style 302 on the PLAYLIST is chased to the media
/// playlist (exercises the redirect loop end-to-end).
#[tokio::test]
async fn playlist_redirects_are_followed() {
    init_tls();
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    let media = "#EXTM3U\n#EXTINF:1.0,\nseg0.ts\n#EXT-X-ENDLIST\n".to_string();
    let app = Router::new().route("/final.m3u8", get(move || async move { media.clone() }));
    // axum: attach Location header via response builder
    let app = app.route(
        "/hop0",
        get(|| async {
            axum::response::Response::builder()
                .status(StatusCode::FOUND)
                .header("location", "/final.m3u8")
                .body(axum::body::Body::empty())
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("red.ts");
    // seg0.ts route missing on this server: fetch will 404 — what we
    // assert is that REDIRECT RESOLUTION got us onto the media
    // playlist (a 404 from seg0 proves we parsed it).
    let err = HlsEngine::new()
        .unwrap()
        .download_merge(
            &DownloadJob {
                url: format!("http://{addr}/hop0"),
                sink,
                resume: None,
                expected_total: None,
                mirrors: Vec::new(),
                fetch_base: None,
            },
            std::sync::Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("seg0.ts"),
        "should have resolved redirect then fetched seg0: {err}"
    );
}

/// Encrypted EXT-X-MAP init: the KEY before the MAP applies to the
/// init itself (RFC 8216 §4.3.2.4, R2 P1-1) — merged file starts
/// with DECRYPTED init bytes.
#[tokio::test]
async fn encrypted_map_is_decrypted_at_merge() {
    init_tls();
    use axum::Router;
    use axum::routing::get;
    let init_plain: Vec<u8> = (200u8..216).collect();
    let key = [7u8; 16];
    let iv = [0u8; 16]; // explicit IV=0x…0 in the playlist
    let init_enc = {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(&init_plain)
    };
    // The KEY applies to BOTH the MAP and seg0 (RFC §4.3.2.4): both
    // are served encrypted under the same explicit IV.
    let seg_plain: Vec<u8> = (0u8..32).collect();
    let seg_enc = {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(&seg_plain)
    };
    let app = Router::new()
        .route("/v.m3u8", get(|| async {
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x00000000000000000000000000000000\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1.0,\nseg0.ts\n#EXT-X-ENDLIST\n"
        }))
        .route("/k", get(move || async move { key.to_vec() }))
        .route("/init.mp4", get(move || async move { init_enc.clone() }))
        .route("/seg0.ts", get(move || async move { seg_enc.clone() }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("e.ts");
    let out = HlsEngine::new()
        .unwrap()
        .download_merge(
            &DownloadJob {
                url: format!("http://{addr}/v.m3u8"),
                sink: sink.clone(),
                resume: None,
                expected_total: None,
                mirrors: Vec::new(),
                fetch_base: None,
            },
            std::sync::Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    let bytes = std::fs::read(&sink).unwrap();
    assert_eq!(bytes.len(), 16 + 32);
    assert_eq!(&bytes[..16], &init_plain[..], "init must be decrypted");
    assert_eq!(&bytes[16..], &seg_plain[..], "segment must be decrypted");
    assert_eq!(out.bytes_written, 48);
}

// ---- QA-E2E Bug 1: gap-tolerant salvage on cancel ----

fn parts_dir_of(sink: &std::path::Path) -> std::path::PathBuf {
    let mut s = sink.as_os_str().to_os_string();
    s.push(".parts");
    std::path::PathBuf::from(s)
}

#[tokio::test]
async fn salvage_merges_parts_with_holes_and_cleans_up() {
    // Disk-discovered salvage: seqs {0, 2, 5} (holes at 1/3/4 — the
    // live window slid past them), one stale .tmp, plus init.mp4.
    // Salvage must concatenate init + 0 + 2 + 5 in order, remove the
    // parts dir, and leave no .hls-merging debris.
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("live.ts");
    let parts = parts_dir_of(&sink);
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(parts.join("000000000005.ts"), vec![5u8; 50]).unwrap();
    std::fs::write(parts.join("000000000000.ts"), vec![0u8; 10]).unwrap();
    std::fs::write(parts.join("000000000002.ts"), vec![2u8; 20]).unwrap();
    std::fs::write(parts.join("garbage.tmp"), vec![9u8; 5]).unwrap();
    std::fs::write(parts.join("init.mp4"), vec![7u8; 4]).unwrap();

    let n = HlsEngine::salvage_merge(&sink).await.unwrap();
    assert_eq!(n, 4 + 10 + 20 + 50, "init + present seqs, tmp skipped");

    let mut want = vec![7u8; 4];
    want.extend_from_slice(&[0u8; 10]);
    want.extend_from_slice(&[2u8; 20]);
    want.extend_from_slice(&[5u8; 50]);
    assert_eq!(
        std::fs::read(&sink).unwrap(),
        want,
        "seq order, holes skipped"
    );
    assert!(!parts.exists(), "parts dir removed after salvage");
    assert!(!sink.with_file_name("live.ts.hls-merging").exists());
}

#[tokio::test]
async fn salvage_without_parts_dir_or_with_only_init_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("none.ts");
    assert_eq!(HlsEngine::salvage_merge(&sink).await.unwrap(), 0);
    assert!(!sink.exists(), "no parts dir — no output file");

    let parts = parts_dir_of(&sink);
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(parts.join("init.mp4"), vec![7u8; 4]).unwrap();
    assert_eq!(HlsEngine::salvage_merge(&sink).await.unwrap(), 0);
    assert!(!sink.exists(), "init-only salvage is not a usable file");
}

#[tokio::test]
async fn salvage_replaces_a_stale_sink_atomically() {
    // A previous run left a HALF-WRITTEN sink (crash during rename
    // of an even older salvage, say). Salvage overwrites it with the
    // current parts — the deliverable always reflects the parts.
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("live.ts");
    let parts = parts_dir_of(&sink);
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(&sink, vec![0xEE; 7]).unwrap(); // stale garbage
    std::fs::write(parts.join("000000000001.ts"), vec![1u8; 8]).unwrap();

    let n = HlsEngine::salvage_merge(&sink).await.unwrap();
    assert_eq!(n, 8);
    assert_eq!(std::fs::read(&sink).unwrap(), vec![1u8; 8]);
    assert!(!parts.exists());
}

/// B50: a permanently-404 segment must fail FAST — exactly one
/// request, no 3× retry backoff (~450ms wasted) against a resource
/// that is gone for good.
#[tokio::test]
async fn permanent_404_segment_is_not_retried() {
    init_tls();
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/vod.m3u8",
            get(|| async {
                "#EXTM3U
#EXT-X-TARGETDURATION:4
#EXTINF:4.0,
seg0.ts
#EXTINF:4.0,
gone.ts
#EXT-X-ENDLIST
"
            }),
        )
        .route("/seg0.ts", get(|| async { vec![7u8; 32] }))
        .route(
            "/gone.ts",
            get({
                let hits = hits.clone();
                move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NOT_FOUND
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("out.ts");
    let engine = HlsEngine::new().unwrap();
    let budget = unlimited();
    let err = engine
        .download_merge(
            &job(&format!("http://{addr}/vod.m3u8"), &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &budget,
        )
        .await
        .expect_err("404 segment must fail the download");

    let msg = format!("{err}");
    assert!(msg.contains("404"), "error should carry the status: {msg}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "permanent 404 must NOT be retried (B50)"
    );
}
