//! engine-hls — HLS (RFC 8216) merge downloader.
//!
//! Pipeline: fetch `.m3u8` → (master: pick highest-BANDWIDTH variant,
//! refetch) → media playlist (VOD only) → download every segment
//! into `<sink>.parts/` (atomic per part: `*.tmp` then rename; a
//! present part is always complete → resume is free) → decrypt
//! AES-128 parts in memory during merge → append init segment (fMP4
//! MAP) first → single output file at `job.sink`.
//!
//! Why merge-at-the-end instead of streaming-append: TS/fMP4
//! segments are independently decodable, but the merged FILE must be
//! exactly `init? ++ seg0 ++ seg1 ++ …` — appending live would let a
//! failed segment #7 of 100 leave a hole that looks like progress.
//! Parts-first also gives idempotent resume for free.

pub mod decrypt;
pub mod error;
mod fetch;
pub mod playlist;

pub use error::HlsError;
use fetch::{Fetcher, read_all};
use peregrine_api::budget::BudgetChain;
use peregrine_api::download::{DownloadJob, DownloadOutcome, DownloadProgress, SharedProgressSink};
use peregrine_api::engine::{ProbeFuture, ProtocolEngine};
use peregrine_api::{ApiError, ProbeInfo};
use playlist::{Key, MediaPlaylist, Playlist};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Segments fetched concurrently. Modest on purpose: CDNs serving
/// HLS rate-limit per connection anyway, and the scheduler's
/// max-active-tasks is the real global concurrency story.
pub const DEFAULT_HLS_CONCURRENCY: usize = 4;

pub struct HlsEngine {
    fetcher: Fetcher,
    concurrency: usize,
    /// Tests pin the poll cadence (0.05–0.3s) so stall/cancel
    /// semantics assert in bounded time; production derives it from
    /// TARGETDURATION.
    poll_cadence_override: Option<std::time::Duration>,
}

impl HlsEngine {
    pub fn new() -> Result<Self, ApiError> {
        Ok(Self {
            fetcher: Fetcher::new().map_err(|e| ApiError::Network(e.to_string()))?,
            concurrency: DEFAULT_HLS_CONCURRENCY,
            poll_cadence_override: None,
        })
    }

    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    pub fn with_poll_cadence_secs(mut self, secs: f64) -> Self {
        self.poll_cadence_override = Some(std::time::Duration::from_secs_f64(secs));
        self
    }

    async fn fetch_text(&self, url: &str) -> Result<String, HlsError> {
        let (status, body) = self.fetcher.get(url, None).await?;
        if !status.is_success() {
            return Err(HlsError::Http {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        let bytes = read_all(body).await?;
        String::from_utf8(bytes).map_err(|e| HlsError::BadPlaylist(e.to_string()))
    }

    /// Master → media → VOD check. Returns (playlist, effective URL).
    async fn resolve(&self, url: &str) -> Result<(MediaPlaylist, String), HlsError> {
        let body = self.fetch_text(url).await?;
        match playlist::parse(&body, url)? {
            Playlist::Master(variants) => {
                let best = variants
                    .iter()
                    .max_by_key(|v| v.bandwidth)
                    .ok_or_else(|| HlsError::BadPlaylist("master with no variants".into()))?;
                let vurl = best.uri.clone();
                let body = self.fetch_text(&vurl).await?;
                match playlist::parse(&body, &vurl)? {
                    Playlist::Media(m) => Ok((m, vurl)),
                    Playlist::Master(_) => Err(HlsError::BadPlaylist(
                        "nested master playlists (not in RFC 8216)".into(),
                    )),
                }
            }
            Playlist::Media(m) => Ok((m, url.to_string())),
        }
    }

    fn parts_dir(sink: &Path) -> PathBuf {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".parts");
        PathBuf::from(s)
    }

    fn part_path(dir: &Path, seq: u64, ext: &str) -> PathBuf {
        dir.join(format!("{seq:012}{ext}"))
    }

    /// Full merge download. The scheduler's `HlsAutoPort` wraps this
    /// (registry/budget wiring stays in the port layer, mirroring
    /// `HttpAutoPort` over `HttpEngine::download_auto`).
    pub async fn download_merge(
        &self,
        job: &DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
        budget: &BudgetChain,
    ) -> Result<DownloadOutcome, HlsError> {
        if cancel.is_cancelled() {
            return Err(HlsError::Network("cancelled".into()));
        }
        let (pl, effective_url) = self.resolve(&job.url).await?;

        let dir = Self::parts_dir(&job.sink);
        tokio::fs::create_dir_all(&dir).await?;
        // Sweep stale `*.tmp` from a hard kill: an aborted run can
        // leave sibling partials (dropped futures never clean up).
        // They are never valid — remove before they can inflate the
        // resume base (R2 P2-2/P2-8).
        sweep_tmp(&dir).await;

        // ---- init segment (fMP4 MAP) ----
        // Fetched VERBATIM even when its key is AES-128: decryption
        // happens in-memory at merge (same discipline as segments —
        // plaintext never persists in .parts).
        if let Some(map) = pl.map.clone() {
            let init = dir.join("init.mp4");
            if !init.exists() {
                let tmp = dir.join("init.mp4.tmp");
                self.fetcher
                    .fetch_part(&map.uri, map.byterange, budget, &cancel, &tmp, &init)
                    .await?;
            }
        }

        // ---- keys (dedup by URI) ----
        let mut keys: std::collections::HashMap<String, [u8; 16]> =
            std::collections::HashMap::new();
        let mut key_uris: Vec<String> = Vec::new();
        if let Some(Key::Aes128 { uri, .. }) = pl.map.as_ref().map(|m| &m.key) {
            key_uris.push(uri.clone());
        }
        for seg in &pl.segments {
            if let Key::Aes128 { uri, .. } = &seg.key
                && !key_uris.contains(uri)
            {
                key_uris.push(uri.clone());
            }
        }
        for uri in &key_uris {
            let (status, body) = self.fetcher.get(uri, None).await?;
            if !status.is_success() {
                return Err(HlsError::Http {
                    status: status.as_u16(),
                    url: uri.clone(),
                });
            }
            let bytes = read_all(body).await?;
            let key: [u8; 16] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| HlsError::Decrypt("key is not 16 bytes".into()))?;
            keys.insert(uri.clone(), key);
        }

        // ---- segments: bounded concurrency, one part file each ----
        let total_hint = job.expected_total;
        let _ = &total_hint;
        let report = |n: u64| {
            progress.on_progress(&DownloadProgress {
                bytes_done: n,
                total: total_hint,
            })
        };

        let base = existing_parts_len(&dir).await;
        report(base);

        let (segments, map) = if pl.ended {
            (pl.segments.clone(), pl.map.clone())
        } else {
            self.follow_live(
                &effective_url,
                &pl,
                &dir,
                &cancel,
                budget,
                base,
                &progress,
                total_hint,
            )
            .await?
        };

        let fetcher = self.fetcher.clone();
        use futures::StreamExt as _;
        // Collect the pending futures EXPLICITLY (instead of the
        // iterator-adapter chain): the chain's higher-ranked closure
        // (for<'a> fn(&'a &'0 MediaSegment) -> bool) trips rustc's
        // `FnOnce` generality check once the stream crosses a
        // Box<dyn Future> boundary (auto_download). A plain Vec of
        // cloned-ownership async blocks has no borrowed lifetimes to
        // go wrong.
        type PartFuture =
            std::pin::Pin<Box<dyn futures::Future<Output = Result<u64, HlsError>> + Send>>;
        let mut pending: Vec<PartFuture> = Vec::new();
        for seg in segments.iter() {
            if Self::part_path(&dir, seg.seq, ".ts").exists() {
                continue;
            }
            let fetcher = fetcher.clone();
            let dir = dir.clone();
            let cancel = cancel.clone();
            let budget = budget.clone();
            let seg = seg.clone();
            pending.push(Box::pin(async move {
                let tmp = Self::part_path(&dir, seg.seq, ".ts.tmp");
                let part = Self::part_path(&dir, seg.seq, ".ts");
                fetcher
                    .fetch_part(&seg.uri, seg.byterange, &budget, &cancel, &tmp, &part)
                    .await?;
                let len = tokio::fs::metadata(&part).await?.len();
                Ok(len)
            }));
        }
        let mut inflight = futures::stream::iter(pending).buffer_unordered(self.concurrency);

        let mut total = base;
        while let Some(res) = inflight.next().await {
            match res {
                Ok(len) => {
                    total += len;
                    report(total);
                }
                // Surface cancellation as OUR marker so the port maps
                // it to ApiError::Cancelled (not a task failure).
                Err(e) if fetch::is_cancel(&e) => {
                    drop(inflight);
                    return Err(HlsError::Network("cancelled".into()));
                }
                Err(e) => {
                    drop(inflight);
                    return Err(e);
                }
            }
        }
        drop(inflight);

        // ---- merge: init? ++ seg0 ++ seg1 … ----
        let merged = merge_parts(&dir, &segments, map.as_ref(), &keys, &job.sink).await?;
        // Parts are now the output file; the directory is removable.
        let _ = tokio::fs::remove_dir_all(&dir).await;

        Ok(DownloadOutcome {
            bytes_written: merged,
            total_bytes: Some(merged),
            completed: true,
            final_url: effective_url,
            final_validator: None,
        })
    }

    /// Follow a live (no ENDLIST) playlist until the stream ends.
    ///
    /// Recording semantics: we start where we JOINED — segments that
    /// slid out of the server window before our first fetch are
    /// gone; recording every seq seen from join to end is the
    /// honest product behavior. `recorded` (seq → metadata), not the
    /// final playlist, is the merge source — the final window has
    /// slid past early seqs.
    ///
    /// Poll cadence: `max(target_duration / 2, 2s)` capped at 8s
    /// (§6.2 — never reload more often than segment duration;
    /// half-duration is the standard hls.js cadence). Termination:
    /// ENDLIST (normal), cancel, or `STALL_POLLS` consecutive polls
    /// with nothing new and no ENDLIST (dead upstream — fail loudly
    /// rather than hang forever).
    const STALL_POLLS: u32 = 6;

    // 9 params: (self + url + playlist snapshot + fs/budget/cancel
    // plumbing + progress pair). A config struct would hide the
    // borrow structure without reducing the coupling — the port
    // layer calls this exactly once.
    #[allow(clippy::too_many_arguments)]
    async fn follow_live(
        &self,
        media_url: &str,
        initial: &MediaPlaylist,
        dir: &Path,
        cancel: &CancellationToken,
        budget: &BudgetChain,
        base_bytes: u64,
        progress: &SharedProgressSink,
        total_hint: Option<u64>,
    ) -> Result<(Vec<playlist::MediaSegment>, Option<playlist::MapSegment>), HlsError> {
        let report = |n: u64| {
            progress.on_progress(&DownloadProgress {
                bytes_done: n,
                total: total_hint,
            })
        };
        let cadence = |td: Option<f64>| -> std::time::Duration {
            if let Some(d) = self.poll_cadence_override {
                return d;
            }
            std::time::Duration::from_secs_f64(td.map(|t| t / 2.0).unwrap_or(4.0).clamp(2.0, 8.0))
        };
        let mut poll_every = cadence(initial.target_duration);

        let mut recorded: std::collections::BTreeMap<u64, playlist::MediaSegment> =
            std::collections::BTreeMap::new();
        let map = initial.map.clone();
        let mut total = base_bytes;
        let mut stall: u32 = 0;
        // Transient poll failures (CDN hiccup, 5xx blip) must not
        // kill a recording: tolerate up to 3 CONSECUTIVE resolve
        // errors before giving up; any successful poll resets.
        let mut poll_failures: u32 = 0;
        let mut current = initial.clone();
        let mut interval = tokio::time::interval(poll_every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            if cancel.is_cancelled() {
                return Err(HlsError::Network("cancelled".into()));
            }
            // A NEW map mid-recording (fMP4 live discontinuity)
            // cannot be represented as one concatenated output.
            if let Some(m) = &current.map
                && map.as_ref() != Some(m)
            {
                return Err(HlsError::Unsupported(
                    "live stream switched EXT-X-MAP mid-recording".into(),
                ));
            }
            // Fetch every UNSEEN seq in this poll's window, bounded
            // by engine concurrency, then record them.
            let fresh: Vec<playlist::MediaSegment> = current
                .segments
                .iter()
                .filter(|s| !recorded.contains_key(&s.seq))
                .cloned()
                .collect();
            if fresh.is_empty() {
                if current.ended {
                    break; // ENDLIST and nothing new — done
                }
                stall += 1;
                if stall >= Self::STALL_POLLS {
                    return Err(HlsError::Unsupported(format!(
                        "live stream stalled: no new segments for {} polls",
                        Self::STALL_POLLS
                    )));
                }
            } else {
                stall = 0;
                poll_failures = 0;
                use futures::StreamExt as _;
                type PartFuture =
                    std::pin::Pin<Box<dyn futures::Future<Output = Result<u64, HlsError>> + Send>>;
                let pending: Vec<PartFuture> = fresh
                    .iter()
                    .filter(|s| !Self::part_path(dir, s.seq, ".ts").exists())
                    .map(|seg| {
                        let fetcher = self.fetcher.clone();
                        let dir = dir.to_path_buf();
                        let cancel = cancel.clone();
                        let budget = budget.clone();
                        let seg = (*seg).clone();
                        Box::pin(async move {
                            let tmp = Self::part_path(&dir, seg.seq, ".ts.tmp");
                            let part = Self::part_path(&dir, seg.seq, ".ts");
                            fetcher
                                .fetch_part(&seg.uri, seg.byterange, &budget, &cancel, &tmp, &part)
                                .await?;
                            let len = tokio::fs::metadata(&part).await?.len();
                            Ok(len)
                        }) as PartFuture
                    })
                    .collect();
                let mut inflight =
                    futures::stream::iter(pending).buffer_unordered(self.concurrency);
                while let Some(res) = inflight.next().await {
                    match res {
                        Ok(len) => {
                            total += len;
                            report(total);
                        }
                        Err(e) if fetch::is_cancel(&e) => {
                            drop(inflight);
                            return Err(HlsError::Network("cancelled".into()));
                        }
                        Err(e) => {
                            drop(inflight);
                            return Err(e);
                        }
                    }
                }
                drop(inflight);
                for seg in fresh {
                    recorded.insert(seg.seq, seg);
                }
                if current.ended {
                    break; // ENDLIST: trailing batch fetched, done
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(HlsError::Network("cancelled".into()));
                }
                _ = interval.tick() => {}
            }
            // R2 P1: the tick branch may win the race with a cancel
            // that fired during the wait — re-check BEFORE resolving
            // so a cancelled task never starts another round trip.
            if cancel.is_cancelled() {
                return Err(HlsError::Network("cancelled".into()));
            }
            match self.resolve(media_url).await {
                Ok((next, _)) => {
                    poll_failures = 0;
                    current = next;
                    // §6.2: honor a cadence change signaled via TARGETDURATION.
                    let want = cadence(current.target_duration);
                    if want != poll_every {
                        poll_every = want;
                        interval = tokio::time::interval(poll_every);
                        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        interval.tick().await; // consume the immediate first tick
                    }
                }
                Err(e) if fetch::is_cancel(&e) => {
                    return Err(HlsError::Network("cancelled".into()));
                }
                Err(e) => {
                    poll_failures += 1;
                    if poll_failures >= 3 {
                        tracing::warn!("live poll failed {}x, giving up", poll_failures);
                        return Err(e);
                    }
                    tracing::debug!("live poll failed ({e}), retrying next cadence");
                }
            }
        }
        Ok((recorded.into_values().collect(), map))
    }
}

/// Bytes already sitting in COMPLETE part files (a resume run
/// reports from here so the task row's `received` never regresses).
/// Counts FINAL names only — `.ts` parts and `init.mp4`; stale
/// `.tmp` partials are debris, not progress (R2 P2-8).
async fn existing_parts_len(dir: &Path) -> u64 {
    let mut n = 0;
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".tmp")
                && let Ok(md) = entry.metadata().await
            {
                n += md.len();
            }
        }
    }
    n
}

/// Remove every `*.tmp` under `dir` (best-effort).
async fn sweep_tmp(dir: &Path) {
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if entry.file_name().to_string_lossy().ends_with(".tmp") {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
}

/// Concatenate parts in PLAYLIST ORDER (not directory order),
/// decrypting AES-128 parts in memory as they stream through. The
/// MAP init is decrypted too when its key says so (RFC 8216 §4.3.2.4,
/// R2 P1-1).
async fn merge_parts(
    dir: &Path,
    segments: &[playlist::MediaSegment],
    map: Option<&playlist::MapSegment>,
    keys: &std::collections::HashMap<String, [u8; 16]>,
    sink: &Path,
) -> Result<u64, HlsError> {
    use tokio::io::AsyncWriteExt;

    // ---- contiguity: a hole means a LOST segment ----
    // (live: the window slid past a seq we never fetched — VOD seqs
    // are contiguous by construction, so this only ever fires for
    // live recordings; merging around the hole would corrupt).
    if let Some(first) = segments.first() {
        for (seg, expect) in segments.iter().zip(first.seq..) {
            if seg.seq != expect {
                return Err(HlsError::BadPlaylist(format!(
                    "segment gap: expected seq {expect}, have {} (window slid past a segment we never fetched)",
                    seg.seq
                )));
            }
        }
    }

    // APPEND `.hls-merging` (never with_extension: `x.ts` and `x.mp4`
    // in one dir would collide on the replaced form — R2 P2-3).
    let out_tmp = {
        let mut s = sink.as_os_str().to_os_string();
        s.push(".hls-merging");
        std::path::PathBuf::from(s)
    };
    let mut out = tokio::fs::File::create(&out_tmp).await?;
    let mut total: u64 = 0;

    let result = merge_inner(dir, segments, map, keys, &mut out, &mut total).await;
    match result {
        Ok(()) => {
            out.flush().await?;
            drop(out);
            tokio::fs::rename(&out_tmp, sink).await?;
            Ok(total)
        }
        Err(e) => {
            // Clean the partial merge so abandoned tasks don't leave
            // debris (R2 P2-3); the .parts stay for resume.
            drop(out);
            let _ = tokio::fs::remove_file(&out_tmp).await;
            Err(e)
        }
    }
}

async fn merge_inner(
    dir: &Path,
    segments: &[playlist::MediaSegment],
    map: Option<&playlist::MapSegment>,
    keys: &std::collections::HashMap<String, [u8; 16]>,
    out: &mut tokio::fs::File,
    total: &mut u64,
) -> Result<(), HlsError> {
    use tokio::io::AsyncWriteExt;

    if let Some(map) = map {
        let init = dir.join("init.mp4");
        let mut bytes = tokio::fs::read(&init).await?;
        if let Key::Aes128 { uri, iv } = &map.key {
            let key = keys
                .get(uri.as_str())
                .ok_or_else(|| HlsError::Decrypt("map key vanished mid-merge".into()))?;
            // The MAP has no media-sequence of its own; an explicit
            // IV is required for encrypted inits — absent IV falls
            // back to 0 (same default §5.2.1.1 gives seq-less uses).
            let iv = iv.unwrap_or([0u8; 16]);
            decrypt::decrypt_cbc(&mut bytes, key, &iv)?;
        }
        out.write_all(&bytes).await?;
        *total += bytes.len() as u64;
    }

    let mut buf: Vec<u8>;
    for seg in segments {
        let part = HlsEngine::part_path(dir, seg.seq, ".ts");
        buf = tokio::fs::read(&part).await?;
        if let Key::Aes128 { uri, iv } = &seg.key {
            let key = keys
                .get(uri.as_str())
                .ok_or_else(|| HlsError::Decrypt("key vanished mid-merge".into()))?;
            let iv = iv.unwrap_or_else(|| decrypt::seq_iv(seg.seq));
            decrypt::decrypt_cbc(&mut buf, key, &iv)?;
        }
        out.write_all(&buf).await?;
        *total += buf.len() as u64;
    }
    Ok(())
}

impl ProtocolEngine for HlsEngine {
    fn name(&self) -> &'static str {
        "hls"
    }

    fn supports(&self, url: &str) -> bool {
        is_hls_url(url)
    }

    fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>> {
        let fetcher = self.fetcher.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (status, body) = match fetcher.get(&url, None).await {
                Ok(r) => r,
                Err(e) => return Err(ApiError::Network(e.to_string())),
            };
            if !status.is_success() {
                return Err(ApiError::Http {
                    status: status.as_u16(),
                    url,
                });
            }
            let bytes = match read_all(body).await {
                Ok(b) => b,
                Err(e) => return Err(ApiError::Network(e.to_string())),
            };
            let body = match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    return Err(ApiError::UnsupportedUrl(format!(
                        "hls probe: {url} is not text (binary?)"
                    )));
                }
            };
            if !body.trim_start().starts_with("#EXTM3U") {
                return Err(ApiError::UnsupportedUrl(format!(
                    "hls probe: {url} is not an m3u8"
                )));
            }
            let filename = playlist_name(&body, &url);
            Ok(ProbeInfo {
                url,
                content_length: None, // playlist size ≠ media size
                accept_ranges: false,
                etag: None,
                etag_strong: false,
                last_modified: None,
                filename,
            })
        })
    }
}

/// URL-shape heuristic (M4-a): the path ends in `.m3u8`
/// (case-insensitive) or carries `m3u8` somewhere outside a `.ts` /
/// `.m4s` path. The DAEMON routes by this before any network I/O;
/// a wrong guess costs one fetch and a clean BadPlaylist error,
/// never a corrupted download. Single definition — the daemon's
/// RoutingPort and this engine's `supports` must never diverge
/// (R2 P2-1).
pub fn is_hls_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower.split('?').next().unwrap_or("");
    path.ends_with(".m3u8")
        || (lower.contains("m3u8") && !path.ends_with(".ts") && !path.ends_with(".m4s"))
}

/// Best-effort output filename: the variant/media playlist's last
/// segment URI stem, else "stream.ts" — the user never keeps the
/// `.m3u8` name.
fn playlist_name(_body: &str, url: &str) -> Option<String> {
    let base = url.split('?').next().unwrap_or(url);
    let stem = base.rsplit('/').next()?;
    let stem = stem.strip_suffix(".m3u8").unwrap_or(stem);
    if stem.is_empty() {
        return None;
    }
    Some(format!("{stem}.ts"))
}
