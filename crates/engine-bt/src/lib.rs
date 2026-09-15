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
    fn register(&mut self, url: &str, id: usize, output_folder: PathBuf) -> Result<(), ApiError> {
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
                        output_folder,
                        urls,
                    },
                );
            }
        }
        self.url_to_id.insert(url.to_string(), id);
        Ok(())
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

        // Same torrent, different folder ⇒ refuse instead of
        // silently rerouting data (R2 F3).
        self.registry
            .lock()
            .unwrap()
            .register(&job.url, id, output_folder_path)?;

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
            // data, if asked). Errors are fine — the torrent may
            // not be in this session at all (fresh daemon, old
            // task row).
            Detach::Last(id) => {
                if let Some(session) = self.session.get() {
                    let _ = session.delete(TorrentIdOrHash::Id(id), purge_files).await;
                }
            }
            // Siblings still hold the torrent: nothing may be
            // deleted (R2' P0-1 — this arm used to fall through to
            // the hash fallback and delete a live sibling's data).
            Detach::Held => {}
            // Unknown url (cross-restart row or foreign source):
            // the hash fallback is allowed ONLY when no tracked url
            // still maps to the same torrent (R2' P0-1).
            Detach::Unknown => {
                if purge_files {
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
