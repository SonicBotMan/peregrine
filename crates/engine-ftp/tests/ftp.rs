//! Integration tests against a scripted in-process FTP server.
//!
//! The mock speaks just enough RFC 959 for the engine: banner,
//! USER/PASS, TYPE, SIZE, REST, PASV + RETR, QUIT — with knobs for
//! auth rejection, missing files, short bodies, and throttled data
//! (cancel-mid-transfer). One server instance per test; every
//! assertion rides a real TCP control + data round trip.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use peregrine_api::ApiError;
use peregrine_api::budget::{BudgetChain, RateBudget};
use peregrine_api::download::{DownloadJob, NoProgress, ResumeContext};
use peregrine_api::engine::ProtocolEngine;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

// ---- mock server ----

#[derive(Clone, Default)]
struct MockSpec {
    files: HashMap<String, Vec<u8>>,
    /// Reject PASS with 530 after asking for it (331 on USER).
    auth_reject: bool,
    /// RETR sends fewer bytes than SIZE announced (short body).
    short_body_by: Option<u64>,
    /// Extra delay per data chunk (cancel-mid-transfer tests).
    chunk_delay: Duration,
    /// Answer 550 to SIZE (server without SIZE support) while RETR
    /// still serves — exercises the unvalidated-resume policy.
    no_size: bool,
}

struct MockFtp {
    spec: MockSpec,
    rest_offset: u64,
    data_listener: Option<TcpListener>,
}

impl MockFtp {
    async fn handle(mut self, control: TcpStream) {
        let (read_half, mut w) = control.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let _ = w.write_all(b"220 mock ftp ready\r\n").await;
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                return; // client closed
            }
            let trimmed = line.trim_end().to_string();
            let (cmd, arg) = match trimmed.split_once(' ') {
                Some((c, a)) => (c.to_ascii_uppercase(), a.to_string()),
                None => (trimmed.to_ascii_uppercase(), String::new()),
            };
            match cmd.as_str() {
                "USER" => {
                    if self.spec.auth_reject {
                        send_raw(&mut w, "331 need password").await;
                    } else {
                        send_raw(&mut w, "230 logged in").await;
                    }
                }
                "PASS" => send_raw(&mut w, "530 login incorrect").await,
                "TYPE" => send_raw(&mut w, "200 type set").await,
                "SIZE" => {
                    let hit = self.spec.files.get(&arg).map(|f| f.len());
                    match (self.spec.no_size, hit) {
                        (false, Some(nn)) => send_raw(&mut w, &format!("213 {nn}")).await,
                        _ => send_raw(&mut w, "550 no size").await,
                    }
                }
                "REST" => {
                    self.rest_offset = arg.parse().unwrap_or(0);
                    send_raw(&mut w, "350 restart at offset").await;
                }
                "PASV" => {
                    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let port = l.local_addr().unwrap().port();
                    self.data_listener = Some(l);
                    send_raw(
                        &mut w,
                        &format!(
                            "227 entering passive (127,0,0,1,{},{})",
                            port >> 8,
                            port & 0xff
                        ),
                    )
                    .await;
                }
                "RETR" => {
                    let Some(listener) = self.data_listener.take() else {
                        send_raw(&mut w, "425 no pasv").await;
                        continue;
                    };
                    let Some(content) = self.spec.files.get(&arg).cloned() else {
                        send_raw(&mut w, "550 no such file").await;
                        continue;
                    };
                    send_raw(&mut w, "150 opening data").await;
                    let (data, _) = listener.accept().await.unwrap();
                    let offset = self.rest_offset.min(content.len() as u64) as usize;
                    let mut payload: &[u8] = &content[offset..];
                    if let Some(short) = self.spec.short_body_by {
                        let want = payload.len().saturating_sub(short as usize);
                        payload = &payload[..want];
                    }
                    let (mut dr, mut dw) = data.into_split();
                    for chunk in payload.chunks(4096) {
                        let _ = dw.write_all(chunk).await;
                        if !self.spec.chunk_delay.is_zero() {
                            tokio::time::sleep(self.spec.chunk_delay).await;
                        }
                    }
                    let _ = dw.shutdown().await;
                    // Drain until EOF so the client close is clean.
                    let mut sink = [0u8; 64];
                    while dr.read(&mut sink).await.unwrap_or(0) > 0 {}
                    self.rest_offset = 0;
                    send_raw(&mut w, "226 transfer complete").await;
                }
                "QUIT" => {
                    send_raw(&mut w, "221 bye").await;
                    return;
                }
                _ => send_raw(&mut w, "502 not implemented").await,
            }
        }
    }
}

async fn send_raw(w: &mut tokio::net::tcp::OwnedWriteHalf, s: &str) {
    let _ = w.write_all(format!("{s}\r\n").as_bytes()).await;
}

/// Boot a mock server; returns `ftp://127.0.0.1:{port}` base.
async fn boot(spec: MockSpec) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let spec = spec.clone();
            tokio::spawn(async move {
                MockFtp {
                    spec,
                    rest_offset: 0,
                    data_listener: None,
                }
                .handle(stream)
                .await
            });
        }
    });
    format!("ftp://{addr}")
}

fn spec_with(file: &str, bytes: Vec<u8>) -> MockSpec {
    let mut files = HashMap::new();
    files.insert(file.to_string(), bytes);
    MockSpec {
        files,
        ..MockSpec::default()
    }
}

fn content(n: u64) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
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

fn engine() -> peregrine_engine_ftp::FtpEngine {
    peregrine_engine_ftp::FtpEngine::new()
}

// ---- tests ----

#[tokio::test]
async fn downloads_file_end_to_end() {
    let body = content(200_000);
    let base = boot(spec_with("/big.bin", body.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("big.bin");

    let out = engine()
        .download(
            job(&format!("{base}/big.bin"), &sink),
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    assert!(out.completed);
    assert_eq!(out.bytes_written, 200_000);
    assert_eq!(out.total_bytes, Some(200_000));
    assert_eq!(std::fs::read(&sink).unwrap(), body);
}

#[tokio::test]
async fn probe_reports_size_and_filename() {
    let base = boot(spec_with("/dir/file.tar.gz", content(1234))).await;
    let info = engine()
        .probe(&format!("{base}/dir/file.tar.gz"))
        .await
        .unwrap();
    assert_eq!(info.content_length, Some(1234));
    assert_eq!(info.filename.as_deref(), Some("file.tar.gz"));
    assert_eq!(info.url, format!("{base}/dir/file.tar.gz"));
}

#[tokio::test]
async fn missing_file_fails_cleanly() {
    let base = boot(spec_with("/there.bin", content(10))).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("x.bin");
    let err = engine()
        .download(
            job(&format!("{base}/missing.bin"), &sink),
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("RETR"), "{err}");
    assert!(!sink.exists() || std::fs::read(&sink).unwrap().is_empty());
}

#[tokio::test]
async fn auth_rejection_surfaces_as_error() {
    let mut spec = spec_with("/f.bin", content(50));
    spec.auth_reject = true;
    let base = boot(spec).await;
    let dir = tempfile::tempdir().unwrap();
    let err = engine()
        .download(
            job(&format!("{base}/f.bin"), &dir.path().join("f.bin")),
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("login"), "{err}");
}

#[tokio::test]
async fn resume_uses_rest_and_appends() {
    let body = content(100_000);
    let base = boot(spec_with("/r.bin", body.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("r.bin");
    // Half a file on disk from a "previous session".
    std::fs::write(&sink, &body[..50_000]).unwrap();
    let mut j = job(&format!("{base}/r.bin"), &sink);
    j.resume = Some(ResumeContext {
        start_offset: 50_000,
        validator: None,
    });

    let out = engine()
        .download(
            j,
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    assert_eq!(out.bytes_written, 50_000, "only the remainder this session");
    assert_eq!(std::fs::read(&sink).unwrap(), body);
}

#[tokio::test]
async fn remote_shrank_restarts_from_zero() {
    let body = content(40_000);
    let base = boot(spec_with("/s.bin", body.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("s.bin");
    // Stale partial LONGER than the current remote file.
    std::fs::write(&sink, vec![0u8; 60_000]).unwrap();
    let mut j = job(&format!("{base}/s.bin"), &sink);
    j.resume = Some(ResumeContext {
        start_offset: 60_000,
        validator: None,
    });

    let out = engine()
        .download(
            j,
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    assert_eq!(out.bytes_written, 40_000, "full replay after shrink");
    assert_eq!(std::fs::read(&sink).unwrap(), body);
}

#[tokio::test]
async fn resume_without_size_support_restarts() {
    // SIZE-less server: the prefix cannot be validated — engine
    // policy is restart-from-zero, never a blind append.
    let mut spec = spec_with("/nosize.bin", content(1000));
    spec.no_size = true;
    let base = boot(spec).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("nosize.bin");
    std::fs::write(&sink, content(400)).unwrap();
    let mut j = job(&format!("{base}/nosize.bin"), &sink);
    j.resume = Some(ResumeContext {
        start_offset: 400,
        validator: None,
    });

    let out = engine()
        .download(
            j,
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap();
    assert_eq!(out.bytes_written, 1000, "restart from zero");
    assert_eq!(std::fs::read(&sink).unwrap(), content(1000));
}
#[tokio::test]
async fn cancel_mid_transfer_leaves_partial() {
    let body = content(400_000);
    let mut spec = spec_with("/slow.bin", body.clone());
    spec.chunk_delay = Duration::from_millis(20); // ~82ms per 16KiB
    let base = boot(spec).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("slow.bin");

    let cancel = CancellationToken::new();
    let j = job(&format!("{base}/slow.bin"), &sink);
    let handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            engine()
                .download(j, Arc::new(NoProgress), cancel, &unlimited())
                .await
        })
    };
    // Wait for the first flushed byte instead of a fixed sleep: on
    // a loaded CI box 300ms may pass before the engine writes
    // anything, which would flake on !partial.is_empty() (R2 P2-10).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&sink).map(|m| m.len()).unwrap_or(0) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "engine wrote nothing in 10s — test environment broken"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cancel.cancel();
    let res = handle.await.unwrap();
    assert!(matches!(res, Err(ApiError::Cancelled)), "got {res:?}");
    let partial = std::fs::read(&sink).unwrap();
    assert!(
        !partial.is_empty() && partial.len() < 400_000,
        "partial of {} bytes retained",
        partial.len()
    );
    assert_eq!(&partial[..], &body[..partial.len()], "prefix intact");
}

#[tokio::test]
async fn short_body_is_an_error_not_completion() {
    let mut spec = spec_with("/short.bin", content(10_000));
    spec.short_body_by = Some(3_000);
    let base = boot(spec).await;
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("short.bin");
    let err = engine()
        .download(
            job(&format!("{base}/short.bin"), &sink),
            Arc::new(NoProgress),
            CancellationToken::new(),
            &unlimited(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("short body"), "{err}");
}

#[tokio::test]
async fn cancelled_token_short_circuits() {
    let base = boot(spec_with("/f.bin", content(10))).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let res = engine()
        .download(
            job(
                &format!("{base}/f.bin"),
                &std::env::temp_dir().join("nope.bin"),
            ),
            Arc::new(NoProgress),
            cancel,
            &unlimited(),
        )
        .await;
    assert!(matches!(res, Err(ApiError::Cancelled)));
}
