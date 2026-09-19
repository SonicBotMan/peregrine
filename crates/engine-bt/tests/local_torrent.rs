//! M4-b2 integration tests: the BT engine against a LOCAL
//! `.torrent` file — no network, no peers. What these cover:
//!
//! - routing predicate (`is_bt_source`) over every source dialect
//! - the add path: local `.torrent` → metadata (total known) →
//!   poll loop → cancel→pause (the "no peers" state is exactly the
//!   steady state of a cancelled-then-resumed magnet, so the
//!   timing windows here are realistic, not synthetic)
//! - **offline completion**: pre-seeding the output file makes
//!   `overwrite` verification credit every piece → `finished`
//!   without a single peer. This is the strongest assertion
//!   available without P2P: it exercises add + verify + stats +
//!   outcome mapping end-to-end.
//! - unpause self-heal: a cancelled download pauses the torrent;
//!   a second download of the SAME torrent must un-pause it and
//!   keep polling (not hang silently).
//! - purge semantics for the file-shaped sink.

use std::sync::Arc;
use std::time::Duration;

use librqbit::create_torrent;
use peregrine_api::download::DownloadJob;
use peregrine_api::{ApiError, DownloadProgress, ProgressSink};
use peregrine_engine_bt::BtEngine;
use tokio_util::sync::CancellationToken;

/// Channel-backed sink: records every base declaration and the
/// LAST progress frame.
type Frames = Vec<(u64, Option<u64>)>;

/// Workspace feature unification links two rustls providers (see
/// scheduler::tls); install ours before any TLS consumer runs.
fn init_tls() {
    peregrine_scheduler::tls::init_tls();
}

#[derive(Clone, Default)]
struct Recorder {
    tx: std::sync::Arc<std::sync::Mutex<Frames>>,
    base: std::sync::Arc<std::sync::atomic::AtomicU64>,
    last: std::sync::Arc<std::sync::Mutex<Option<DownloadProgress>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            tx: Default::default(),
            base: Default::default(),
            last: Default::default(),
        }
    }

    fn last(&self) -> Option<DownloadProgress> {
        *self.last.lock().unwrap()
    }

    fn frames(&self) -> Vec<(u64, Option<u64>)> {
        self.tx.lock().unwrap().clone()
    }
}

impl ProgressSink for Recorder {
    fn on_progress(&self, p: &DownloadProgress) {
        self.tx.lock().unwrap().push((p.bytes_done, p.total));
        *self.last.lock().unwrap() = Some(*p);
    }

    fn on_session_base(&self, base: u64) {
        self.base.store(base, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Build a local `.torrent` for a random file; returns
/// (torrent_path, file_bytes, name).
async fn make_torrent(
    dir: &std::path::Path,
    size: usize,
) -> (std::path::PathBuf, Vec<u8>, String, String) {
    let name = "seed.bin";
    let bytes: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
    std::fs::write(dir.join(name), &bytes).unwrap();

    let result = create_torrent(
        &dir.join(name),
        Default::default(),
        &librqbit::spawn_utils::BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    let torrent_path = dir.join("seed.torrent");
    std::fs::write(&torrent_path, result.as_bytes().unwrap()).unwrap();
    let magnet = result.as_magnet().to_string();
    (torrent_path, bytes, name.to_string(), magnet)
}

fn job_for(torrent: &std::path::Path, out_dir: &std::path::Path) -> DownloadJob {
    DownloadJob {
        url: format!("file://{}", torrent.display()),
        sink: out_dir.join("file.bin"),
        resume: None,
        expected_total: None,
        mirrors: Vec::new(),
        fetch_base: None,
    }
}

async fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
#[test]
fn bt_source_routing() {
    init_tls();
    assert!(peregrine_engine_bt::is_bt_source("magnet:?xt=urn:btih:abc"));
    assert!(peregrine_engine_bt::is_bt_source("http://x/1.torrent"));
    assert!(peregrine_engine_bt::is_bt_source("https://X/1.TORRENT"));
    assert!(peregrine_engine_bt::is_bt_source("/tmp/1.torrent"));
    assert!(peregrine_engine_bt::is_bt_source("file:///tmp/1.torrent"));
    // Query strings / fragments must not break suffix routing (R2 F5).
    assert!(peregrine_engine_bt::is_bt_source(
        "http://x/1.torrent?passkey=k"
    ));
    assert!(peregrine_engine_bt::is_bt_source(
        "https://x/1.torrent#frag"
    ));
    assert!(!peregrine_engine_bt::is_bt_source(
        "http://x/1.bin?x=.torrent"
    ));
    assert!(!peregrine_engine_bt::is_bt_source("http://x/1.bin"));
    assert!(!peregrine_engine_bt::is_bt_source("ftp://x/1.torrent"));
    assert!(!peregrine_engine_bt::is_bt_source("/tmp/1.bin"));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_torrent_metadata_then_cancel() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;

    let engine = BtEngine::offline();
    let rec = Recorder::new();
    let cancel = CancellationToken::new();

    let handle = tokio::spawn({
        let engine_job = job_for(&torrent, out.path());
        let rec = rec.clone();
        let cancel = cancel.clone();
        async move {
            engine
                .download(engine_job, Arc::new(rec) as _, cancel)
                .await
        }
    });

    // Metadata from the .torrent: total becomes known without any
    // peer exchange.
    assert!(
        wait_until(Duration::from_secs(20), || rec
            .last()
            .is_some_and(|p| p.total == Some(bytes.len() as u64)))
        .await,
        "expected total from torrent metadata; frames: {:?}",
        rec.frames()
    );

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("download must return after cancel")
        .unwrap();
    assert!(matches!(result, Err(ApiError::Cancelled)));

    // No data was written (no peers) — the engine's output stays
    // empty or sparse; nothing to assert on file contents.
    let _ = name;
}

#[tokio::test(flavor = "multi_thread")]
async fn local_torrent_completes_offline_from_verified_data() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;

    // Pre-seed the complete payload: overwrite verification will
    // credit every piece → finished, zero bytes written this
    // session, zero peers needed.
    std::fs::write(out.path().join(&name), &bytes).unwrap();

    let engine = BtEngine::offline();
    let rec = Recorder::new();
    let cancel = CancellationToken::new();

    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        engine.download(
            job_for(&torrent, out.path()),
            Arc::new(rec.clone()) as _,
            cancel,
        ),
    )
    .await
    .expect("verified torrent must finish (not hang)")
    .unwrap();

    assert_eq!(outcome.total_bytes, Some(bytes.len() as u64));
    assert!(outcome.completed);
    // bytes_written here races rqbit's async piece verification:
    // if verification finished BEFORE `add_torrent` returned, the
    // session base is already full-size (0 "written"); if after,
    // the verified bytes are counted as this session's progress
    // (= full size). Both are legitimate; what must hold: never
    // more than the total, and the payload intact on disk.
    assert!(outcome.bytes_written <= bytes.len() as u64);
    assert_eq!(
        std::fs::read(out.path().join(&name)).unwrap(),
        bytes,
        "verified payload must be intact"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_torrent_resumes_and_self_heals() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, _name, _magnet) = make_torrent(src.path(), 256 * 1024).await;

    let engine = Arc::new(BtEngine::offline());

    // First download: cancelled while waiting for peers → pause.
    let cancel_a = CancellationToken::new();
    let task_a = tokio::spawn({
        let j = job_for(&torrent, out.path());
        let cancel = cancel_a.clone();
        let engine = engine.clone();
        async move {
            engine
                .download(j, Arc::new(Recorder::new()) as _, cancel)
                .await
        }
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    cancel_a.cancel();
    assert!(matches!(task_a.await.unwrap(), Err(ApiError::Cancelled)));

    // Second download, same torrent: AlreadyManaged → unpause →
    // the poll loop must keep ticking (self-heal re-asserts
    // unpause if a late pause lands). Must NOT hang.
    let rec_b = Recorder::new();
    let cancel_b = CancellationToken::new();
    let task_b = tokio::spawn({
        let j = job_for(&torrent, out.path());
        let engine = engine.clone();
        let cancel = cancel_b.clone();
        let sink = Arc::new(rec_b.clone()) as _;
        async move { engine.download(j, sink, cancel).await }
    });

    assert!(
        wait_until(Duration::from_secs(30), || rec_b
            .last()
            .is_some_and(|p| p.total == Some(bytes.len() as u64)))
        .await,
        "second download must attach and observe metadata; frames: {:?}",
        rec_b.frames()
    );

    cancel_b.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), task_b)
        .await
        .expect("second download must be responsive to cancel (self-healed unpause)")
        .unwrap();
    assert!(matches!(result, Err(ApiError::Cancelled)));
}

#[tokio::test(flavor = "multi_thread")]
async fn purge_is_idempotent_on_missing_files() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, _bytes, _name, _magnet) = make_torrent(src.path(), 256 * 1024).await;

    let engine = BtEngine::offline();
    // Fresh engine: no session yet (never downloaded) — purge must
    // still work (missing sink = Ok).
    engine
        .purge(
            &format!("file://{}", torrent.display()),
            &out.path().join("file.bin"),
            true,
        )
        .await
        .unwrap();

    // And again with a REAL file at the sink: removed, then the
    // second purge is NotFound→Ok.
    let sink = out.path().join("file.bin");
    std::fs::write(&sink, b"x").unwrap();
    engine
        .purge(&format!("file://{}", torrent.display()), &sink, true)
        .await
        .unwrap();
    assert!(!sink.exists());
    engine
        .purge(&format!("file://{}", torrent.display()), &sink, true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn purge_after_completion_removes_data_and_session_entry() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    std::fs::write(out.path().join(&name), &bytes).unwrap();

    let engine = BtEngine::offline();
    let url = format!("file://{}", torrent.display());

    // Complete offline (verification of pre-seeded data).
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        engine.download(
            job_for(&torrent, out.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("verified torrent must finish")
    .unwrap();
    assert!(outcome.completed);

    // Purge with the registry populated: session entry + DATA both
    // go away (R2 F2 — non-magnet sources purge for real).
    engine
        .purge(&url, &out.path().join("file.bin"), true)
        .await
        .unwrap();
    assert!(
        !out.path().join(&name).exists(),
        "purge must delete the torrent data"
    );

    // Re-add after delete: a fresh session entry (not
    // AlreadyManaged-paused), completes again from re-verification.
    // (purge may remove the output folder itself when the torrent
    // owns it — recreate it before re-seeding.)
    std::fs::create_dir_all(out.path()).unwrap();
    std::fs::write(out.path().join(&name), &bytes).unwrap();
    let outcome2 = tokio::time::timeout(
        Duration::from_secs(60),
        engine.download(
            job_for(&torrent, out.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("re-add after purge must work")
    .unwrap();
    assert!(outcome2.completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn same_torrent_into_second_folder_is_rejected() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out_a = tempfile::tempdir().unwrap();
    let out_b = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    std::fs::write(out_a.path().join(&name), &bytes).unwrap();

    let engine = BtEngine::offline();

    // Task A completes into folder A.
    let outcome_a = tokio::time::timeout(
        Duration::from_secs(60),
        engine.download(
            job_for(&torrent, out_a.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("task A must finish")
    .unwrap();
    assert!(outcome_a.completed);

    // Task B: same torrent, DIFFERENT folder. The engine must
    // refuse (sink contract) instead of silently attaching to A's
    // session entry (R2 F3) — data would land in A's folder.
    let err = tokio::time::timeout(
        Duration::from_secs(30),
        engine.download(
            job_for(&torrent, out_b.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("task B must return promptly")
    .unwrap_err();
    assert!(
        matches!(&err, ApiError::InvalidInput(msg) if msg.contains("already downloading")),
        "expected folder-conflict refusal, got: {err:?}"
    );
    // And folder B stayed empty.
    assert!(!out_b.path().join(&name).exists());
}

/// R2' P0-2 regression: two tasks sharing ONE url (same torrent,
/// same folder, different sink filenames — the task-manager guard
/// only blocks identical (url, save_path)). The first purge must
/// NOT delete the shared data; only the last reference does.
#[tokio::test]
async fn same_url_two_sinks_refcounts_purge() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    let engine = BtEngine::offline();

    // Task A completes from pre-seeded data.
    std::fs::write(out.path().join(&name), &bytes).unwrap();
    let mut job_a = job_for(&torrent, out.path());
    job_a.sink = out.path().join("a.bin");
    let out_a = tokio::time::timeout(
        Duration::from_secs(30),
        engine.download(
            job_a.clone(),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("task A must finish")
    .unwrap();
    assert!(out_a.completed);

    // Task B: same url, same folder, different sink name. Attaches
    // to the live entry and re-verifies to completion.
    let mut job_b = job_for(&torrent, out.path());
    job_b.sink = out.path().join("b.bin");
    let out_b = tokio::time::timeout(
        Duration::from_secs(30),
        engine.download(
            job_b.clone(),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("task B must finish")
    .unwrap();
    assert!(out_b.completed);

    // First purge of the SHARED url: a sibling still holds it —
    // data and session entry must survive.
    engine.purge(&job_a.url, &job_a.sink, true).await.unwrap();
    assert!(
        out.path().join(&name).exists(),
        "sibling purge deleted shared data"
    );
    assert!(
        engine.session_entry_alive(),
        "sibling purge killed live entry"
    );

    // Second purge of the same url: last reference — data goes.
    engine.purge(&job_b.url, &job_b.sink, true).await.unwrap();
    assert!(!out.path().join(&name).exists(), "last purge kept data");
    assert!(!engine.session_entry_alive(), "last purge kept entry");
}

/// R2' P0-1 regression: purging an UNKNOWN url whose magnet hash
/// resolves to a torrent a LIVE task still references must not
/// delete that torrent's data (the hash fallback is guarded by
/// the registry's holds_id check).
#[tokio::test]
async fn magnet_hash_fallback_respects_live_sibling() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, magnet) = make_torrent(src.path(), 256 * 1024).await;
    let engine = BtEngine::offline();

    // Live task via the .torrent file url (registry holds it).
    std::fs::write(out.path().join(&name), &bytes).unwrap();
    let job = job_for(&torrent, out.path());
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        engine.download(
            job.clone(),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("task must finish")
    .unwrap();
    assert!(outcome.completed);

    // Purge the MAGNET url — never registered (Detach::Unknown) —
    // with purge_files=true. Same infohash as the live task: the
    // hash fallback MUST decline and leave the sibling's data.
    let stranger_sink = out.path().join("stranger.bin");
    engine.purge(&magnet, &stranger_sink, true).await.unwrap();
    assert!(
        out.path().join(&name).exists(),
        "hash fallback deleted a live sibling's data"
    );
    assert!(
        engine.session_entry_alive(),
        "hash fallback killed live entry"
    );
}

// ---------------------------------------------------------------------
// B59: cross-restart purge via the persisted url→folder side table.

#[tokio::test(flavor = "multi_thread")]
async fn b59_cross_restart_purge_deletes_data_folder_via_side_table() {
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let reg = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    let reg_path = reg.path().join("bt-registry.json");

    // Session 1: complete the torrent (verified re-check) — the
    // engine registers url→folder in memory AND in the side table.
    std::fs::write(out.path().join(&name), &bytes).unwrap();
    let e1 = BtEngine::offline().with_registry_persistence(reg_path.clone());
    let url = format!("file://{}", torrent.display());
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        e1.download(
            job_for(&torrent, out.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("verified torrent must finish (not hang)")
    .unwrap();
    assert!(outcome.completed);
    // The side table must now exist and map the url to the
    // torrent's PRECISE data path (output_folder.join(name)) —
    // NEVER the shared output folder (B59 R2 P0-1: deleting it
    // could take unrelated files in the same directory with it).
    let table: std::collections::HashMap<String, std::path::PathBuf> =
        serde_json::from_slice(&std::fs::read(&reg_path).unwrap()).unwrap();
    assert_eq!(table.len(), 1, "side table: {table:?}");
    assert_eq!(
        table[&url],
        out.path().join(&name),
        "side table must record the precise data path, not the output folder"
    );

    // "Restart": new engine instance — in-memory registry empty,
    // rqbit session gone; ONLY the side table survives.
    drop(e1);
    let e2 = BtEngine::offline().with_registry_persistence(reg_path.clone());
    assert!(
        out.path().join(&name).exists(),
        "data still on disk before purge"
    );
    // An unrelated file shares the output folder — it must
    // survive the purge (P0-1 collateral-deletion regression).
    let bystander = out.path().join("stranger.txt");
    std::fs::write(&bystander, "unrelated").unwrap();

    // Purge of the pre-restart row (file:// url — no magnet hash
    // to derive): without B59 the fallback only removed the sink
    // path and the data folder survived forever.
    let sink = out.path().join("file.bin");
    e2.purge(&url, &sink, true).await.unwrap();

    assert!(
        !out.path().join(&name).exists(),
        "B59: cross-restart purge must delete the torrent's data"
    );
    assert!(
        bystander.exists(),
        "B59 R2 P0-1: purge must NOT delete the shared output folder"
    );
    let table: std::collections::HashMap<String, std::path::PathBuf> =
        serde_json::from_slice(&std::fs::read(&reg_path).unwrap()).unwrap();
    assert!(table.is_empty(), "side table entry consumed: {table:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn b59_side_table_entry_freed_on_regular_purge() {
    // A NORMAL (same-session) purge must also consume the side
    // table entry — otherwise the next cross-restart purge of the
    // same url would resurrect a delete on a folder the user may
    // have re-created.
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let reg = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    let reg_path = reg.path().join("bt-registry.json");

    std::fs::write(out.path().join(&name), &bytes).unwrap();
    let engine = BtEngine::offline().with_registry_persistence(reg_path.clone());
    let url = format!("file://{}", torrent.display());
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        engine.download(
            job_for(&torrent, out.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("verified torrent must finish (not hang)")
    .unwrap();
    assert!(outcome.completed);

    // Same-session purge (registry HAS the url → Last/Held path,
    // not Unknown): side table entry must go too.
    engine
        .purge(&url, &out.path().join("file.bin"), true)
        .await
        .unwrap();
    let table: std::collections::HashMap<String, std::path::PathBuf> =
        serde_json::from_slice(&std::fs::read(&reg_path).unwrap()).unwrap();
    assert!(
        table.is_empty(),
        "same-session purge consumed entry: {table:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn b59_keep_files_purge_neither_deletes_data_nor_consumes_entry() {
    // purge_files=false (remove_keep_files semantics): the data
    // stays on disk BY CONTRACT, so its cross-restart locator must
    // stay too — consuming the entry would downgrade a future
    // purge to the sink fallback (B59 R2 P1-1). And the data
    // itself must not be deleted by the side-table hit.
    init_tls();
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let reg = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    let reg_path = reg.path().join("bt-registry.json");

    std::fs::write(out.path().join(&name), &bytes).unwrap();
    let e1 = BtEngine::offline().with_registry_persistence(reg_path.clone());
    let url = format!("file://{}", torrent.display());
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        e1.download(
            job_for(&torrent, out.path()),
            Arc::new(Recorder::new()) as _,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("verified torrent must finish (not hang)")
    .unwrap();
    assert!(outcome.completed);

    // "Restart", then a keep-files purge: neither the data nor the
    // side-table entry may be touched.
    drop(e1);
    let e2 = BtEngine::offline().with_registry_persistence(reg_path.clone());
    e2.purge(&url, &out.path().join("file.bin"), false)
        .await
        .unwrap();
    assert!(
        out.path().join(&name).exists(),
        "keep-files purge deleted data via side table"
    );
    let table: std::collections::HashMap<String, std::path::PathBuf> =
        serde_json::from_slice(&std::fs::read(&reg_path).unwrap()).unwrap();
    assert_eq!(table.len(), 1, "keep-files purge kept the entry: {table:?}");
}

/// Peers deep-link, no-network arm: an unknown url is `None` (the
/// port layer turns that into `bt:false` for the REST surface), and
/// a KNOWN url (offline completion path) yields `bt:true` with the
/// session state — the peers list itself is empty offline, which is
/// the honest answer, not a stub.
#[tokio::test(flavor = "multi_thread")]
async fn peers_snapshot_offline_semantics() {
    init_tls();
    let engine = BtEngine::offline();
    assert!(
        engine
            .peers("magnet:?xt=urn:btih:0000000000000000000000000000000000000000")
            .is_none(),
        "untracked url → None (REST: bt:false)"
    );

    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (torrent, bytes, name, _magnet) = make_torrent(src.path(), 256 * 1024).await;
    std::fs::write(out.path().join(&name), &bytes).unwrap();

    let rec = Recorder::new();
    let cancel = CancellationToken::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        engine.download(
            job_for(&torrent, out.path()),
            Arc::new(rec.clone()) as _,
            cancel,
        ),
    )
    .await
    .expect("verified torrent must finish (not hang)")
    .unwrap();
    assert!(outcome.completed, "pre-seeded data verifies instantly");

    // job_for uses a file:// url — peers() must resolve the SAME
    // url the engine registered.
    let url = format!("file://{}", torrent.display());
    let snap = engine.peers(&url).expect("tracked url → Some");
    assert!(snap.bt, "BT task snapshot reports bt:true");
    assert_eq!(
        snap.total_bytes,
        bytes.len() as u64,
        "metadata known after completion"
    );
    assert!(snap.peers.is_empty(), "offline → zero live peers");
}
