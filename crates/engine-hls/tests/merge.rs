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
    }
}

#[tokio::test]
async fn merges_master_aes_and_map_end_to_end() {
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

#[tokio::test]
async fn live_playlist_is_rejected() {
    use axum::Router;
    use axum::routing::get;
    let app = Router::new().route(
        "/live.m3u8",
        get(|| async { "#EXTM3U\n#EXTINF:4.0,\na.ts\n" }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("live.ts");
    let err = HlsEngine::new()
        .unwrap()
        .download_merge(
            &job(&format!("http://{addr}/live.m3u8"), &sink),
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("live"), "{err}");
}

#[tokio::test]
async fn probe_validates_m3u8_and_names_output() {
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
