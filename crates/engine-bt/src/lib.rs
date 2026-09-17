//! BitTorrent engine over [librqbit] (M4-b2). PROPOSAL §4.3: the
//! BT protocol's complexity is not worth rewriting — librqbit is
//! embedded as a library and hidden behind the same
//! engine/port split as HTTP/FTP/HLS.
//!
//! ## Design
//!
//! - **One `Session` per process** (DHT + peer listener are shared
//!   infrastructure; per-torrent sessions would fork the DHT
//!   network state per download). Lazily initialized inside
//!   `download` so merely linking this crate costs nothing.
//! - **DHT**: enabled WITHOUT persistence in production
//!   (`DhtSessionConfig::persistence: None` — magnets need DHT to
//!   find peers, but persisting the routing table would write a
//!   global `~/.cache/dht.json` behind the user's back; peregrine
//!   owns durable state, rqbit stays stateless).
//!   [`BtEngine::offline`] disables DHT entirely (tests, pure
//!   local setups — no network at all).
//! - **Output folder** is derived per-job from `job.sink`'s parent:
//!   for BT the sink is the DOWNLOAD DIRECTORY, not the final file
//!   name — a torrent's file names come from its metadata and are
//!   not renamable without breaking piece hashes. Single-file
//!   torrents land in `<parent>/<torrent-name>`; multi-file ones
//!   in `<parent>/<torrent-name>/…` (librqbit semantics, kept
//!   as-is rather than reimplemented).
//! - **Resume**: librqbit re-verifies existing pieces when adding
//!   with `overwrite: true`, so a re-added magnet resumes from
//!   verified data with zero peregrine-side state. An in-session
//!   re-add returns `AlreadyManaged` and simply re-attaches.
//! - **Cancel** maps to `Session::pause` (files stay on disk; the
//!   next add re-verifies). There is no peregrine "delete" here —
//!   file deletion is the port's `purge`.
//! - **Registry**: every successful add records
//!   `url → TorrentId` plus per-id metadata (output folder, url
//!   set). `purge` uses it to delete the session entry for
//!   NON-magnet sources too (whose hash cannot be re-derived from
//!   the url), and to refuse two tasks fighting over the same
//!   torrent in different folders. The registry dies with the
//!   process; cross-restart purges fall back to magnet-hash
//!   derivation, and to sink removal as a last resort.
//!
//! v1 deltas (BACKLOG): no budget wiring (BT throttling needs
//! librqbit's runtime limits API — the port surfaces a loud warn
//! instead of silently pretending); no seeding policy knobs
//! (leech-until-complete then stay available for the session's
//! lifetime — rqbit's default); sink paths with non-UTF-8 bytes go
//! through lossy `display()` (daemon-validated paths are UTF-8 in
//! practice).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, Session, SessionOptions,
};
use peregrine_api::ApiError;
use peregrine_api::download::{DownloadJob, DownloadOutcome, DownloadProgress, SharedProgressSink};
use tokio_util::sync::CancellationToken;

/// Consecutive ticks a vanished torrent may go unnoticed before we
/// give up (300ms tick × 10 ≈ 3s of grace for transient races).
const VANISHED_GRACE_TICKS: u32 = 10;

/// Fallback download root for torrents whose sink has no parent
/// (never in practice — the daemon validates paths — but Session
/// requires SOME default folder at construction).
fn fallback_output_folder() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".peregrine/bt")
}

static FALLBACK_DIR: LazyLock<PathBuf> = LazyLock::new(fallback_output_folder);

/// Does this source string belong to the BT engine? `magnet:`
/// scheme, or a `.torrent` file (local path OR http(s)/file URL).
/// URL judgement goes through `Url::path()` so query strings and
/// fragments (`...file.torrent?passkey=x`) don't break routing.
///
/// Single routing authority (M4-a lesson): the daemon's RoutingPort
/// calls THIS, never re-implements the heuristic.
pub fn is_bt_source(s: &str) -> bool {
    if let Ok(u) = url::Url::parse(s) {
        return u.scheme() == "magnet"
            || (matches!(u.scheme(), "http" | "https" | "file")
                && u.path().to_ascii_lowercase().ends_with(".torrent"));
    }
    // Local filesystem path.
    s.to_ascii_lowercase().ends_with(".torrent")
}

fn add_source(job: &DownloadJob) -> Result<AddTorrent<'static>, ApiError> {
    // Route through Url::parse like is_bt_source does (R2' P2-3):
    // parse lowercases the scheme, so MAGNET:/HTTP://X/1.TORRENT
    // can't diverge from the router's verdict and fall into the
    // local-path branch with a misleading error.
    if let Ok(u) = url::Url::parse(&job.url) {
        match u.scheme() {
            "magnet" | "http" | "https" => return Ok(AddTorrent::from_url(job.url.clone())),
            "file" => {
                if let Ok(p) = u.to_file_path() {
                    return AddTorrent::from_local_filename(&p.display().to_string())
                        .map_err(|e| ApiError::UnsupportedUrl(format!("bad .torrent path: {e}")));
                }
            }
            _ => {}
        }
    }
    // Bare local path.
    AddTorrent::from_local_filename(&job.url)
        .map_err(|e| ApiError::UnsupportedUrl(format!("bad .torrent path: {e}")))
}

/// Per-torrent bookkeeping: which folder the data lives in and
/// which task urls reference it (two tasks on the same torrent
/// share one session entry — purging one must not kill the other).
#[derive(Default)]
struct Registry {
    url_to_id: HashMap<String, usize>,
    id_meta: HashMap<usize, TorrentMeta>,
    /// B59: cross-restart purge locator — url → output_folder,
    /// persisted to disk (json) beside the task DB. The in-memory
    /// maps above die with the process (session ids are
    /// meaningless after a restart); this side table is what lets
    /// a purge of a pre-restart `.torrent` row still find (and
    /// delete) the data directory, which neither magnet-hash
    /// derivation nor the sink path can locate.
    persist_path: Option<PathBuf>,
    folders: HashMap<String, PathBuf>,
}

impl Registry {
    /// B59: load the persisted url→precise-data-path table
    /// (best-effort: a corrupt file ⇒ empty table + warn —
    /// cross-restart purges degrade to the sink fallback, nothing
    /// else breaks).
    fn load_persisted(path: PathBuf) -> Self {
        let folders = match std::fs::read(&path) {
            Ok(b) if b.is_empty() => Default::default(),
            Ok(b) => match serde_json::from_slice::<HashMap<String, PathBuf>>(&b) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "BT registry side-table corrupt — starting empty");
                    Default::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "reading BT registry side-table failed");
                Default::default()
            }
        };
        Self {
            persist_path: Some(path),
            folders,
            ..Default::default()
        }
    }

    /// Rewrite the persistence file atomically (tmp + rename — a
    /// crash mid-write must not zero the table, B59 R2 P2-1).
    /// Best-effort with a warn: a failed flush only downgrades a
    /// FUTURE cross-restart purge to the sink-path fallback. Note:
    /// blocking IO inside the registry mutex, called from async —
    /// acceptable at tens-of-entries scale.
    fn flush(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        match serde_json::to_vec(&self.folders) {
            Ok(bytes) => {
                if let Err(e) =
                    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path))
                {
                    tracing::warn!(error = %e, path = %path.display(), "flushing BT registry side-table failed");
                    let _ = std::fs::remove_file(&tmp);
                }
            }
            Err(e) => tracing::warn!(error = %e, "serializing BT registry side-table failed"),
        }
    }
}

#[derive(Default)]
struct TorrentMeta {
    output_folder: PathBuf,
    /// url → refcount (R2' P0-2): two tasks may legitimately share
    /// ONE url (same magnet, same folder, different sink filenames
    /// — the task-manager dedup guard only blocks identical
    /// (url, save_path) pairs). A set would under-count: the first
    /// remove would look like the last reference.
    urls: HashMap<String, usize>,
}

impl Registry {
    /// Record a successful add. Returns an error if the SAME
    /// torrent is already downloading into a DIFFERENT folder —
    /// silently rerouting would break the second task's sink
    /// contract (its data would land in the first task's folder).
    ///
    /// `data_path` is the torrent's PRECISE on-disk entity
    /// (`output_folder.join(torrent_name)` — the multi-file top
    /// dir or the single file itself). It is what the cross-restart
    /// purge deletes — NEVER the shared `output_folder`, which
    /// other tasks' data may live in too (B59 R2 P0-1). `None`
    /// (unresolved magnet) simply skips the side table: a leaked
    /// folder is acceptable, collateral deletion is not.
    fn register(
        &mut self,
        url: &str,
        id: usize,
        output_folder: PathBuf,
        data_path: Option<PathBuf>,
    ) -> Result<(), ApiError> {
        match self.id_meta.get_mut(&id) {
            Some(meta) if meta.output_folder != output_folder => {
                return Err(ApiError::InvalidInput(format!(
                    "same torrent already downloading into {} (task {} would land elsewhere)",
                    meta.output_folder.display(),
                    url
                )));
            }
            Some(meta) => {
                *meta.urls.entry(url.to_string()).or_insert(0) += 1;
            }
            None => {
                let mut urls = HashMap::new();
                urls.insert(url.to_string(), 1);
                self.id_meta.insert(
                    id,
                    TorrentMeta {
                        output_folder: output_folder.clone(),
                        urls,
                    },
                );
            }
        }
        self.url_to_id.insert(url.to_string(), id);
        // B59: record the PRECISE data path durably — a purge after
        // a daemon restart has no other way to find it. Only when
        // known (resolved metadata).
        if let Some(dp) = data_path {
            self.folders.insert(url.to_string(), dp);
            self.flush();
        }
        Ok(())
    }

    /// Consume a side-table entry AFTER the data it pointed at was
    /// actually deleted (B59 R2 P1-2: deleting the mapping before
    /// the delete succeeds would turn a transient EACCES into a
    /// permanent leak — scheduler retries can't resurrect a row it
    /// already removed).
    fn consume_side_entry(&mut self, url: &str) {
        if self.folders.remove(url).is_some() {
            self.flush();
        }
    }

    /// Resolve a torrent locator for purging, falling back to
    /// magnet hash derivation (cross-restart rows have no registry
    /// entry).
    fn resolve(&self, url: &str) -> Option<TorrentIdOrHash> {
        if let Some(id) = self.url_to_id.get(url) {
            return Some(TorrentIdOrHash::Id(*id));
        }
        magnet_locator(url).ok()
    }

    /// Whether any tracked url still maps to `id` (R2' P0-1): the
    /// magnet-hash purge fallback must not delete a session entry
    /// that a surviving task still references.
    fn holds_id(&self, id: usize) -> bool {
        self.url_to_id.values().any(|&v| v == id)
    }

    /// Detach one reference (R2' P0-1: three-state — a plain
    /// Option<usize> cannot distinguish "unknown url" from
    /// "siblings still hold it", and conflating them is what let
    /// the magnet-hash fallback delete a live sibling's data).
    fn detach(&mut self, url: &str) -> Detach {
        // Peek, don't remove: the url→id mapping must survive while
        // ANY task still references the torrent — a later purge of
        // the same url has to resolve to the same entry (dropping
        // the mapping on a Held outcome sent every follow-up purge
        // into the Unknown/hash-fallback arm; found via the
        // same_url_two_sinks regression).
        let Some(id) = self.url_to_id.get(url).copied() else {
            return Detach::Unknown;
        };
        let last = self
            .id_meta
            .get_mut(&id)
            .map(|meta| {
                let c = meta.urls.get_mut(url).map(|c| {
                    *c -= 1;
                    *c
                });
                match c {
                    Some(0) => {
                        meta.urls.remove(url);
                        meta.urls.is_empty()
                    }
                    Some(_) => false, // another task holds the same url
                    None => meta.urls.is_empty(),
                }
            })
            .unwrap_or(true);
        if last {
            self.url_to_id.remove(url);
            self.id_meta.remove(&id);
            // NOTE: the B59 side-table entry is intentionally NOT
            // removed here — detach is about the session mapping;
            // the data locator is consumed only when the data is
            // actually deleted (purge's Last/Unknown arms).
            Detach::Last(id)
        } else {
            Detach::Held
        }
    }
}

/// Outcome of dropping one task's reference to a torrent.
enum Detach {
    /// No registry entry for this url (cross-restart row or foreign
    /// source) — hash fallback territory, guarded by `holds_id`.
    Unknown,
    /// Other tasks still reference the torrent: nothing may be
    /// deleted from the session.
    Held,
    /// This was the final reference; the session entry (and data,
    /// if asked) may go.
    Last(usize),
}

pub struct BtEngine {
    /// One Session per ENGINE (daemon holds one engine ⇒ one
    /// process-wide session; tests can build isolated ones). Lazy:
    /// linking the engine costs nothing until the first download.
    session: tokio::sync::OnceCell<Arc<Session>>,
    registry: Mutex<Registry>,
    /// Production keeps DHT (magnets need it); `offline()` clears
    /// it for tests/pure-local setups.
    dht: bool,
}

impl BtEngine {
    /// Production engine: DHT on, table NOT persisted (no global
    /// `~/.cache/dht.json` side effects — peregrine owns durable
    /// state).
    pub fn new() -> Self {
        Self {
            session: tokio::sync::OnceCell::const_new(),
            registry: Mutex::new(Registry::default()),
            dht: true,
        }
    }

    /// B59: persist the url→data-folder side table so purges of
    /// pre-restart `.torrent` rows can still locate (and delete)
    /// their data. Loads any existing table immediately.
    pub fn with_registry_persistence(mut self, path: PathBuf) -> Self {
        self.registry = Mutex::new(Registry::load_persisted(path));
        self
    }

    /// No-network engine: DHT disabled entirely. For tests and
    /// strictly-local .torrent workflows.
    pub fn offline() -> Self {
        Self {
            session: tokio::sync::OnceCell::const_new(),
            registry: Mutex::new(Registry::default()),
            dht: false,
        }
    }

    async fn session(&self) -> Result<Arc<Session>, ApiError> {
        self.session
            .get_or_try_init(|| async {
                // Structural TLS guard (M4-b2 R2 F7): librqbit's
                // internal reqwest does NOT pass through
                // engine-http's https_client(), so the ring
                // provider may still be uninstalled when this crate
                // is embedded directly. Idempotent + race-safe.
                let _ = rustls::crypto::ring::default_provider().install_default();
                let dht = self.dht.then(|| DhtSessionConfig {
                    persistence: None,
                    ..Default::default()
                });
                // offline() means NO network at all (R2' P2-2):
                // with only DHT off, librqbit still fires LSD UDP
                // multicasts and tracker announces per torrent.
                let (disable_lsd, disable_trackers) = (!self.dht, !self.dht);
                Session::new_with_opts(
                    FALLBACK_DIR.clone(),
                    SessionOptions {
                        // No session persistence: peregrine's task
                        // table is the durable record; rqbit state is
                        // rebuilt by piece re-verification on re-add.
                        persistence: None,
                        dht,
                        disable_local_service_discovery: disable_lsd,
                        disable_trackers,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| ApiError::Internal(format!("bt session init failed: {e}")))
            })
            .await
            .map(Arc::clone)
    }

    pub async fn download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Result<DownloadOutcome, ApiError> {
        let session = self.session().await?;

        let output_folder = job
            .sink
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| fallback_output_folder().display().to_string());
        let output_folder_path = PathBuf::from(&output_folder);

        let source = add_source(&job)?;
        let opts = AddTorrentOptions {
            output_folder: Some(output_folder),
            // Re-verify + write on top of existing files: this IS
            // the resume mechanism for BT.
            overwrite: true,
            ..Default::default()
        };

        let response = session
            .add_torrent(source, Some(opts))
            .await
            .map_err(|e| map_bt_error("add_torrent", e))?;

        // Keep BOTH the numeric id (purge/registry/alive checks) and
        // the handle (stats/pause). into_handle() drops the id.
        let (id, handle) = match response {
            AddTorrentResponse::Added(id, h) | AddTorrentResponse::AlreadyManaged(id, h) => (id, h),
            AddTorrentResponse::ListOnly(_) => {
                return Err(ApiError::UnsupportedUrl(
                    "torrent source produced no handle (list_only?)".into(),
                ));
            }
        };

        // B59: the PRECISE data entity = output_folder.join(torrent
        // name). None while a magnet is still resolving (name unknown
        // yet) — then the side table simply doesn't cover this row
        // and cross-restart purge degrades to the sink fallback.
        let data_path = session
            .get(TorrentIdOrHash::Id(id))
            .and_then(|t| t.name())
            .filter(|n| !n.is_empty())
            .map(|n| output_folder_path.join(n));

        // Same torrent, different folder ⇒ refuse instead of
        // silently rerouting data (R2 F3).
        self.registry
            .lock()
            .unwrap()
            .register(&job.url, id, output_folder_path, data_path)?;

        // AlreadyManaged → resume in place (unpause so the download
        // actually continues after a previous cancel-pause).
        if handle.is_paused() {
            session
                .unpause(&handle)
                .await
                .map_err(|e| map_bt_error("unpause", e))?;
        }

        // Baseline: verified-but-incomplete bytes BEFORE this
        // session does anything (overwrite re-verification may have
        // credited prior partial data — that's resume, not progress).
        let start = handle.stats().progress_bytes;
        progress.on_session_base(start);

        let mut poll = tokio::time::interval(Duration::from_millis(300));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut vanished = 0u32;
        let outcome = loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    // Pause (not delete): files + session entry stay,
                    // re-adding the same magnet resumes in-place.
                    session.pause(&handle).await.ok();
                    return Err(ApiError::Cancelled);
                }
                _ = poll.tick() => {
                    // Vanish detection (R2 F3): a purge of a sibling
                    // task (or any session.delete) removes the entry
                    // out from under us. Without this check the poll
                    // loop would spin forever on a ghost handle —
                    // stats never error, never finish, never pause.
                    if session.get(TorrentIdOrHash::Id(id)).is_none() {
                        vanished += 1;
                        if vanished >= VANISHED_GRACE_TICKS {
                            return Err(ApiError::Internal(
                                "torrent vanished from session (purged concurrently?)".into(),
                            ));
                        }
                        continue;
                    }
                    vanished = 0;
                    // Self-heal: re-assert unpause every tick. A
                    // concurrent download of the SAME torrent being
                    // cancelled pauses it under our feet (one
                    // session, shared torrent); without this the
                    // surviving download would stall silently.
                    if handle.is_paused() {
                        session.unpause(&handle).await.ok();
                    }
                    let stats = handle.stats();
                    if let Some(err) = &stats.error {
                        return Err(map_bt_error("torrent", err));
                    }
                    progress.on_progress(&DownloadProgress {
                        bytes_done: stats.progress_bytes,
                        total: (stats.total_bytes > 0).then_some(stats.total_bytes),
                    });
                    if stats.finished {
                        break Ok(DownloadOutcome {
                            bytes_written: stats.progress_bytes.saturating_sub(start),
                            total_bytes: Some(stats.total_bytes),
                            completed: true,
                            final_url: job.url.clone(),
                            final_validator: None,
                            replayed_from_zero: false,
                        });
                    }
                }
            }
        };
        // Dropping the handle drops only our POLLING reference —
        // the torrent stays live in the session (leech-until-done,
        // then seed for the session's lifetime, rqbit default).
        drop(handle);
        outcome
    }

    /// Test/ops probe: is any torrent alive in the (lazily created)
    /// session? Offline integration tests assert purge semantics
    /// through this (no other way to observe the session from
    /// outside the crate).
    pub fn session_entry_alive(&self) -> bool {
        match self.session.get() {
            Some(s) => s.with_torrents(|torrents| torrents.count() > 0),
            None => false,
        }
    }

    /// Remove engine-side state for (url, sink): the torrent's
    /// session entry and — if `purge_files` — the downloaded data.
    ///
    /// Registry-first: the recorded `url → id` mapping makes this
    /// work for NON-magnet sources too (their hash can't be derived
    /// from the url). When several tasks share one torrent, only
    /// the LAST url's purge deletes the entry/data — purging one
    /// task never destroys another's live download (R2 F2/F3).
    /// Cross-restart rows (empty registry) fall back to magnet
    /// hash derivation; the sink removal below is the final
    /// fallback for .torrent files with no other handle.
    pub async fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
        purge_files: bool,
    ) -> anyhow::Result<()> {
        let outcome = {
            let mut reg = self.registry.lock().unwrap();
            reg.detach(url)
        };
        match outcome {
            // Final reference gone: delete the session entry (and
            // data, if asked). The side-table entry is consumed
            // ONLY after a successful data delete (B59 R2 P1-2) —
            // and NEVER on purge_files=false, where the data stays
            // on disk and so must its locator (B59 R2 P1-1: same
            // semantics as the Unknown path below).
            Detach::Last(id) => {
                if let Some(session) = self.session.get()
                    && session
                        .delete(TorrentIdOrHash::Id(id), purge_files)
                        .await
                        .is_ok()
                    && purge_files
                {
                    self.registry.lock().unwrap().consume_side_entry(url);
                }
            }
            // Siblings still hold the torrent: nothing may be
            // deleted (R2' P0-1 — this arm used to fall through to
            // the hash fallback and delete a live sibling's data).
            Detach::Held => {}
            // Unknown url (cross-restart row or foreign source):
            // the B59 side table first — it holds the torrent's
            // PRECISE data path (top-level dir/file), never the
            // shared output folder (B59 R2 P0-1: an earlier draft
            // remove_dir_all'd the parent and could take
            // ~/Downloads with it). A hit deletes exactly that path
            // and RETURNS — the same data has exactly one correct
            // delete target, the by-hash/sink fallbacks are for
            // rows the table never knew (B59 R2 P1-3). The hash
            // fallback is allowed ONLY when no tracked url still
            // maps to the same torrent (R2' P0-1).
            Detach::Unknown => {
                if purge_files {
                    let side_path = self.registry.lock().unwrap().folders.get(url).cloned();
                    if let Some(data_path) = side_path {
                        remove_sink(&data_path).await?;
                        // Deleted successfully → NOW consume the
                        // entry; on failure the `?` above returns
                        // with the entry intact for the next try.
                        self.registry.lock().unwrap().consume_side_entry(url);
                        return Ok(());
                    }
                    let by_hash = self.registry.lock().unwrap().resolve(url);
                    let id_alive = by_hash
                        .as_ref()
                        .and_then(|locator| {
                            self.session.get().and_then(|s| match locator {
                                TorrentIdOrHash::Id(id) => Some(*id),
                                // Hash → look the live entry up to
                                // learn its session id; a miss means
                                // no live entry to protect.
                                TorrentIdOrHash::Hash(h) => {
                                    s.get(TorrentIdOrHash::Hash(*h)).map(|h| h.id())
                                }
                            })
                        })
                        .is_some_and(|id| self.registry.lock().unwrap().holds_id(id));
                    match (by_hash, id_alive) {
                        (Some(locator), false) if self.session.get().is_some() => {
                            let session = self.session.get().unwrap();
                            let _ = session.delete(locator, true).await;
                        }
                        _ => remove_sink(sink).await?,
                    }
                }
            }
        }
        Ok(())
    }
}

/// Classify a librqbit failure: disk/permission problems are Io,
/// everything else rides as Network (protocol-level by nature).
/// String-matching is coarse but honest about what it knows —
/// rqbit wraps io errors in display strings.
fn map_bt_error(stage: &str, e: impl std::fmt::Display) -> ApiError {
    let s = e.to_string();
    if s.contains("os error") || s.contains("No such file") || s.contains("Permission denied") {
        ApiError::Io(format!("{stage}: {s}"))
    } else {
        ApiError::Network(format!("{stage}: {s}"))
    }
}

async fn remove_sink(sink: &std::path::Path) -> anyhow::Result<()> {
    match tokio::fs::remove_dir_all(sink).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(dir_err) => match tokio::fs::remove_file(sink).await {
            Ok(()) => Ok(()),
            Err(file_err) if file_err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(anyhow::anyhow!("purge {sink:?}: dir: {dir_err}")),
        },
    }
}

/// Extract the info-hash from a magnet URI (v1 btih, hex or base32)
/// as a torrent locator. Non-magnet urls return Err.
fn magnet_locator(url: &str) -> Result<TorrentIdOrHash, anyhow::Error> {
    let parsed = librqbit::Magnet::parse(url)?;
    parsed
        .as_id20()
        .map(TorrentIdOrHash::Hash)
        .ok_or_else(|| anyhow::anyhow!("magnet has no v1 info hash"))
}

impl Default for BtEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// One peer row for the GUI's BT deep-link panel. Cumulative
/// counters (rqbit semantics); the GUI derives instantaneous
/// rates by differencing two snapshots, so the daemon stays a
/// stateless mirror.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BtPeerInfo {
    /// `ip:port` exactly as rqbit keys it.
    pub addr: String,
    /// Self-reported client ("qBittorrent 4.6"…), live peers only.
    pub client: Option<String>,
    /// rqbit state name: "live", "connecting", …
    pub state: String,
    /// Transport: "tcp"/"utp"/"socks" (live peers only).
    pub conn_kind: Option<String>,
    /// Cumulative payload bytes fetched from this peer.
    pub fetched_bytes: u64,
    /// Cumulative bytes uploaded to this peer.
    pub uploaded_bytes: u64,
    /// Connection errors so far.
    pub errors: u32,
}

/// BT deep-link snapshot (`GET /tasks/{id}/peers`). `bt: false`
/// marks a non-BT task: the REST layer answers one panel shape
/// instead of a 404, so the GUI renders uniformly.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BtPeersSnapshot {
    pub bt: bool,
    /// Torrent present in the session (may be paused).
    pub live: bool,
    pub paused: bool,
    /// BT task whose add is still resolving metadata (magnet
    /// before DHT/tracker answers): no session entry yet, so the
    /// panel says "resolving…" instead of a misleading "not BT".
    #[serde(default)]
    pub resolving: bool,
    pub total_bytes: u64,
    pub peers: Vec<BtPeerInfo>,
}

impl BtEngine {
    /// Live peer snapshot for a tracked url (the GUI peers panel,
    /// REST `/tasks/{id}/peers`). `None` when this engine doesn't
    /// know the url — the port layer turns that into `bt: false`
    /// for non-BT tasks and for BT rows whose session entry is
    /// gone (cross-restart: nothing is downloading until re-add).
    /// Sync on purpose: everything it touches is lock/atomic
    /// reads inside rqbit, no async surface to await.
    pub fn peers(&self, url: &str) -> Option<BtPeersSnapshot> {
        let id = *self.registry.lock().unwrap().url_to_id.get(url)?;
        let session = self.session.get()?;
        let handle = session.get(TorrentIdOrHash::Id(id))?;
        let stats = handle.stats();
        let mut snap = BtPeersSnapshot {
            bt: true,
            live: handle.live().is_some(),
            paused: handle.is_paused(),
            resolving: false,
            total_bytes: stats.total_bytes,
            peers: Vec::new(),
        };
        if let Some(live) = handle.live() {
            // ::default() filter = live peers only — rqbit doesn't
            // re-export the filter-state enum, and "connecting"
            // rows carry no data for the panel anyway.
            let raw =
                live.per_peer_stats_snapshot(librqbit::http_api_types::PeerStatsFilter::default());
            snap.peers = raw
                .peers
                .into_iter()
                .map(|(addr, p)| BtPeerInfo {
                    addr,
                    client: p.client_name,
                    state: p.state.to_string(),
                    // ConnectionKind's module path is private in
                    // librqbit; Debug-format is the stable spelling.
                    conn_kind: p.conn_kind.map(|k| format!("{k:?}").to_lowercase()),
                    fetched_bytes: p.counters.fetched_bytes,
                    uploaded_bytes: p.counters.uploaded_bytes,
                    errors: p.counters.errors,
                })
                .collect();
            // Live peers first (the ones actually moving data),
            // then by fetched bytes — the panel's default view.
            snap.peers.sort_by(|a, b| {
                let live_of = |s: &str| (s != "live") as u8;
                live_of(&a.state)
                    .cmp(&live_of(&b.state))
                    .then(b.fetched_bytes.cmp(&a.fetched_bytes))
            });
        }
        Some(snap)
    }
}
