//! Engine-port purge contracts (M5.1 P0-2): data deletion is the
//! caller's EXPLICIT choice — `purge_files=false` must keep user
//! data, `true` must delete it, at every port. Zero-network:
//! purge paths only touch the filesystem.

use peregrine_api::budget::RateBudget;
use peregrine_engine_http::SegmentConfig;
use peregrine_scheduler::{DownloadPort, FtpAutoPort, HlsAutoPort, HttpAutoPort};
use peregrine_storage::Store;

fn with_parts_dir(sink: &std::path::Path) -> std::path::PathBuf {
    let mut s = sink.as_os_str().to_os_string();
    s.push(".parts");
    std::path::PathBuf::from(s)
}

fn http_port() -> HttpAutoPort {
    let engine = std::sync::Arc::new(peregrine_engine_http::HttpEngine::new().unwrap());
    let store = Store::open_memory().unwrap();
    HttpAutoPort::new(
        engine,
        SegmentConfig::default(),
        store,
        RateBudget::unlimited(),
    )
}

#[tokio::test]
async fn http_purge_false_keeps_data_true_deletes() {
    // M5.1 R2' P1-1: the HTTP engine must honor purge_files like
    // every other engine — a no-op here broke the REST/CLI/MCP
    // "deletes partial files" promise for the MAIN engine.
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("h.bin");
    std::fs::write(&sink, b"partial").unwrap();
    let port = http_port();

    // Plain remove: rows cleaned, file kept.
    port.purge("http://x/h.bin", &sink, false).await.unwrap();
    assert!(sink.exists(), "purge_files=false must keep user data");

    // Purged remove: file gone; missing file on a second purge is
    // Ok (idempotent at this layer too).
    port.purge("http://x/h.bin", &sink, true).await.unwrap();
    assert!(!sink.exists(), "purge_files=true must delete user data");
    port.purge("http://x/h.bin", &sink, true).await.unwrap();
}

#[tokio::test]
async fn ftp_purge_false_keeps_data_true_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("f.bin");
    std::fs::write(&sink, b"data").unwrap();
    let port = FtpAutoPort::new(RateBudget::unlimited(), Store::open_memory().unwrap());

    // Plain remove: the sink survives.
    port.purge("ftp://x/f.bin", &sink, false).await.unwrap();
    assert!(sink.exists(), "purge_files=false must keep user data");

    // Purged remove: gone (missing file is Ok — idempotent).
    port.purge("ftp://x/f.bin", &sink, true).await.unwrap();
    assert!(!sink.exists(), "purge_files=true must delete user data");
}

#[tokio::test]
async fn ftp_purge_true_on_missing_file_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("never.bin");
    let port = FtpAutoPort::new(RateBudget::unlimited(), Store::open_memory().unwrap());
    port.purge("ftp://x/never.bin", &sink, true).await.unwrap();
}

#[tokio::test]
async fn hls_purge_false_keeps_parts_true_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let sink = dir.path().join("v.ts");
    let parts = with_parts_dir(&sink);
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(&sink, b"merged").unwrap();
    std::fs::write(parts.join("seg0.ts"), b"seg").unwrap();
    let port = HlsAutoPort::new(RateBudget::unlimited(), Store::open_memory().unwrap()).unwrap();

    // Plain remove: merged output AND parts dir survive.
    port.purge("http://x/v.m3u8", &sink, false).await.unwrap();
    assert!(sink.exists(), "purge_files=false keeps the merged file");
    assert!(parts.exists(), "purge_files=false keeps the parts dir");

    // Purged remove: both gone.
    port.purge("http://x/v.m3u8", &sink, true).await.unwrap();
    assert!(!sink.exists(), "purge_files=true deletes the merged file");
    assert!(!parts.exists(), "purge_files=true deletes the parts dir");
}
