//! Multi-segment download execution (PROPOSAL §5, stages 2–3 + 5.2).
//!
//! M1-c1 scope: static segmentation with a bounded worker pool,
//! per-segment cursor persistence in SQLite, and validator-change
//! detection that wipes the segment set. Dynamic rebalancing (§5.1
//! stage 4) and adaptive concurrency (stage 5) are M1-c2.
//!
//! Invariants the rest of the engine may lean on:
//!
//! * **No-overlap**: planned ranges partition `[0, total)` exactly —
//!   `sum(seg.len()) == total`, `seg[i].end + 1 == seg[i+1].start`.
//! * **Two-ended 206 validation (B20)**: a worker that asked for
//!   `bytes=S-E` refuses any 206 whose Content-Range does not start at
//!   `S` AND end at `E` — no byte from a mismatched range is written.
//! * **Monotone cursors**: `update_cursor` is `MAX(done, n)` in SQL;
//!   a stale worker cannot move a segment backwards.

use crate::download::fetch_get;
use crate::{HttpEngine, HttpsClient};
use http_body_util::BodyExt;
use peregrine_api::download::{DownloadJob, DownloadOutcome, IfRangeValidator};
use peregrine_api::{ApiError, DownloadProgress};
use peregrine_storage::{SegmentState, Store, TaskId};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;

/// Segment size floor: never split a file into pieces smaller than
/// this (PROPOSAL §5.1 stage 2: "段太小(阈值如 5MB)则合并").
const DEFAULT_MIN_SEGMENT_BYTES: u64 = 5 * 1024 * 1024;

/// Default/upper bound on simultaneous segment workers. 8 is the
/// conservative default from the risk table (§4: "默认保守值(8连接)可调");
/// 32 matches the K clamp ceiling.
const DEFAULT_MAX_CONCURRENCY: usize = 8;
const HARD_CONCURRENCY_CEILING: usize = 32;

/// Persist the segment cursor at most every this-many new bytes.
/// Crash cost ≈ this much refetch; SQLite write cost is bounded by
/// the cadence this induces (5 MiB of traffic per tiny UPDATE).
const CURSOR_PERSIST_BYTES: u64 = 1024 * 1024;

/// Per-worker progress callback floor; keeps a 32-worker swarm from
/// flooding the sink when a fast local server feeds 1 GiB/s.
const PROGRESS_EVERY_BYTES: u64 = 256 * 1024;

/// Same stall budget as the single-stream path — a wedged segment
/// worker must not pin the whole task forever.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Engine-side tuning knobs. Thin wrapper over the serde-facing
/// [`peregrine_api::SegmentConfig`] (which the M2 daemon exposes over
/// IPC) — one source of truth for the numbers, an engine shape for
/// the planner.
#[derive(Debug, Clone, Copy)]
pub struct SegmentConfig {
    /// Minimum bytes per planned segment; smaller pieces are merged.
    pub min_segment_bytes: u64,
    /// Maximum simultaneous segment workers (hard ceiling 32).
    pub max_concurrency: usize,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            min_segment_bytes: DEFAULT_MIN_SEGMENT_BYTES,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
        }
    }
}

impl From<peregrine_api::SegmentConfig> for SegmentConfig {
    fn from(c: peregrine_api::SegmentConfig) -> Self {
        // The api side's `sanitized()` already clamps; clamp again so
        // hand-built (non-sanitized) values cannot bypass the ceiling.
        Self {
            min_segment_bytes: c.min_segment.max(1),
            max_concurrency: (c.max_conns as usize).clamp(1, HARD_CONCURRENCY_CEILING),
        }
    }
}

impl SegmentConfig {
    pub fn new(min_segment_bytes: u64, max_concurrency: usize) -> Self {
        Self {
            min_segment_bytes: min_segment_bytes.max(1),
            max_concurrency: max_concurrency.clamp(1, HARD_CONCURRENCY_CEILING),
        }
    }
}

/// Evenly partition `[0, total)` into `K` inclusive ranges.
///
/// `K = min(max_concurrency, ceil(total / min_segment_bytes))`, at
/// least 1. The first `total % K` ranges get one extra byte, so every
/// range length is `floor(total/K)` or `ceil(total/K)` — all ≥ 1.
///
/// ```text
/// plan_ranges(10_000_000, min=1MiB, K=8) → 8 ranges of 1.25 MiB
/// plan_ranges(6_000_000, min=5MiB, K=8)  → 2 ranges of 3 MiB (K clamped)
/// plan_ranges(0, …)                      → [] (nothing to fetch)
/// ```
pub fn plan_ranges(total: u64, cfg: &SegmentConfig) -> Vec<(u64, u64)> {
    if total == 0 {
        return Vec::new();
    }
    // Defense in depth (P2-4, M1-c1 R2): SegmentConfig::new enforces
    // min_segment_bytes ≥ 1, but a hand-constructed struct (it is a
    // plain pub struct) would panic in div_ceil(0) below. Clamp so
    // a bad config degrades to one segment instead of panicking
    // inside a worker.
    let seg_floor = cfg.min_segment_bytes.max(1);
    let by_size = total.div_ceil(seg_floor).max(1);
    let k = by_size.min(cfg.max_concurrency as u64).max(1) as usize;
    let base = total / k as u64;
    let rem = total % k as u64;
    let mut ranges = Vec::with_capacity(k);
    let mut start = 0u64;
    for i in 0..k {
        let len = base + if (i as u64) < rem { 1 } else { 0 };
        let end = start + len - 1; // len ≥ 1 by construction
        ranges.push((start, end));
        start = end + 1;
    }
    debug_assert_eq!(start, total);
    debug_assert!(ranges.iter().map(|&(s, e)| e - s + 1).sum::<u64>() == total);
    ranges
}

/// Segmented download driver. Requires `job.expected_total == Some(_)`
/// (unknown-size downloads must take the single-stream path) and a
/// `Store` for cursor persistence. On success the task row is dropped
/// — the completed file on disk is the truth.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_segmented_download(
    client: &HttpsClient,
    max_redirects: usize,
    job: DownloadJob,
    cfg: &SegmentConfig,
    store: &Store,
    progress: &peregrine_api::download::SharedProgressSink,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<DownloadOutcome, ApiError> {
    // Wire form of the probe-confirmed validator (sent as If-Range on
    // every segment request and compared against the stored one for
    // change detection).
    let probe_validator = job
        .resume
        .as_ref()
        .and_then(|c| c.validator.as_ref())
        .map(|v| match v {
            IfRangeValidator::StrongEtag(e) => e.clone(),
            IfRangeValidator::LastModified(d) => d.clone(),
        });

    // A mid-swarm "If-Range rejected" (worker got a 200 or a mismatched
    // served etag) proves the resource changed; the task restarts ONCE
    // from zero without any validator (the old one describes a dead
    // version). A second rejection surfaces as the error.
    let mut attempt_validator = probe_validator.clone();
    for attempt in 0..2 {
        if attempt == 1 {
            attempt_validator = None;
        }
        match run_attempt(
            client,
            max_redirects,
            &job,
            cfg,
            store,
            progress,
            attempt_validator.clone(),
            attempt > 0,
            &cancel,
        )
        .await
        {
            Ok(out) => return Ok(out),
            Err((e, restart)) if restart && attempt == 0 => {
                tracing::warn!(error = %e, "resource changed mid-download — replanning from zero");
                continue;
            }
            Err((e, _)) => return Err(e),
        }
    }
    unreachable!("attempt loop returns on the second pass")
}

/// One planning + execution pass. `force_fresh` replans unconditionally
/// (restart-after-resource-change); `validator_wire` is the If-Range to
/// send (probe's, else none). The bool in the error is "restart the
/// task from zero" — set only when the evidence says the resource
/// changed under us.
#[allow(clippy::too_many_arguments)]
async fn run_attempt(
    client: &HttpsClient,
    max_redirects: usize,
    job: &DownloadJob,
    cfg: &SegmentConfig,
    store: &Store,
    progress: &peregrine_api::download::SharedProgressSink,
    validator_wire: Option<String>,
    force_fresh: bool,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<DownloadOutcome, (ApiError, bool)> {
    let fatal = |e: ApiError| (e, false);
    let DownloadJob {
        url,
        sink,
        expected_total,
        ..
    } = job;

    let total = expected_total.ok_or_else(|| {
        fatal(ApiError::Network(format!(
            "segmented download requires a known total; {url} did not report one"
        )))
    })?;

    // --- plan or resume (PROPOSAL §5.2) ----------------------------
    // Stored cursors survive ONLY if we can prove they describe the
    // current resource (P0-2, M1-c1 R2): a Range request without
    // If-Range always answers 206, so "the server will tell us via
    // 200" is NOT a staleness signal unless a validator is on the
    // wire. Wipe whenever:
    //   * the total moved, or the stored segment set doesn't cover it
    //     exactly (P2-1: defends the non-atomic upsert/replace window
    //     and manual DB edits),
    //   * both the probe's and the stored validator are known and
    //     differ (two generations, both visible),
    //   * NEITHER is known — without a validator we cannot prove the
    //     cursors belong to the current bytes, so conservatively
    //     replan (same posture as the single-stream 200-truncate).
    // A single-sided validator is fine: it goes out as If-Range and
    // the 206-vs-200 answer settles staleness per segment.
    let existing = store
        .get_task(url, sink)
        .await
        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;
    let stale = !force_fresh
        && existing.as_ref().is_some_and(|t| {
            if t.total != Some(total) {
                return true;
            }
            if t.segments.iter().map(|s| s.len()).sum::<u64>() != total {
                return true;
            }
            match (&t.etag, &validator_wire) {
                (Some(a), Some(b)) => a != b,
                (None, None) => true,
                _ => false,
            }
        });
    if stale || force_fresh {
        tracing::info!("task state unusable (total/validator/cover changed) — replanning");
    }
    // If-Range every worker sends: the probe's validator when present,
    // else the stored one (stored-but-never-sent was half a guard —
    // P0-2's fix is to actually put it on the wire).
    let if_range = if stale || force_fresh {
        validator_wire.clone()
    } else {
        validator_wire
            .clone()
            .or_else(|| existing.as_ref().and_then(|t| t.etag.clone()))
    };

    // --- sink/cursor consistency (P0-1, M1-c1 R2) ------------------
    // Resumed cursors promise bytes are on disk. Prove it: the sink
    // must exist and be exactly `total` long. A missing partial
    // (temp cleaner, user cleanup) replans from zero — the common
    // case auto-recovers; a wrong-length sink means outside
    // interference we will not paper over.
    // No row at all is the plain fresh case — plan from zero. (A
    // missing row with existing_done > 0 cannot happen: that sum is
    // derived from the same row.)
    let mut wipe_needed = force_fresh || existing.is_none() || stale;
    let existing_done: u64 = existing
        .as_ref()
        .map(|t| t.segments.iter().map(|s| s.done).sum())
        .unwrap_or(0);
    if existing_done > 0 && !wipe_needed {
        match tokio::fs::metadata(sink).await {
            Ok(m) if m.len() == total => {}
            Ok(m) => {
                return Err(fatal(ApiError::Io(format!(
                    "resume mismatch: sink {} has {} bytes, task plans for {total} — \
                     delete the partial file or the task state",
                    sink.display(),
                    m.len()
                ))));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("resume target missing — replanning from zero");
                wipe_needed = true;
            }
            Err(e) => return Err(fatal(ApiError::Io(format!("stat {}: {e}", sink.display())))),
        }
    }

    let (task_id, segments): (peregrine_storage::TaskId, Vec<SegmentState>) = if wipe_needed {
        replan(store, url, sink, total, &validator_wire, cfg)
            .await
            .map_err(fatal)?
    } else {
        let t = existing.expect("non-wipe branch implies a row");
        debug_assert_eq!(t.total, Some(total));
        (t.id, t.segments)
    };
    let initial_done: u64 = if wipe_needed { 0 } else { existing_done };
    let done_counter = Arc::new(AtomicU64::new(initial_done));
    let target = DownloadProgress {
        bytes_done: initial_done,
        total: Some(total),
    };
    if initial_done > 0 {
        progress.on_progress(&target);
    }

    // --- sparse preallocation (§5.2) --------------------------------
    // One handle for the metadata op; workers open their own.
    {
        let prealloc = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            // Explicitly NOT truncating: a resumed task's partial
            // bytes on disk must survive the preallocation open.
            .truncate(false)
            .open(sink)
            .await
            .map_err(|e| fatal(ApiError::Io(format!("open {}: {e}", sink.display()))))?;
        prealloc
            .set_len(total)
            .await
            .map_err(|e| fatal(ApiError::Io(format!("preallocate {}: {e}", sink.display()))))?;
    }

    // --- worker pool --------------------------------------------------
    let ctx = SegmentCtx {
        client: client.clone(),
        max_redirects,
        total,
        url: url.clone(),
        sink: sink.clone(),
        task_id,
        validator: if_range,
        store: store.clone(),
        done_counter: done_counter.clone(),
        progress: progress.clone(),
    };
    let permits = Arc::new(Semaphore::new(cfg.max_concurrency));
    let mut handles = Vec::with_capacity(segments.len());
    for seg in &segments {
        if seg.is_complete() {
            continue;
        }
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| fatal(ApiError::Network("segment pool closed".into())))?;
        let ctx = ctx.clone();
        let seg = *seg;
        handles.push(tokio::spawn(async move {
            let _permit = permit; // held for the worker's lifetime
            run_segment(ctx, seg).await
        }));
    }

    // First failure aborts the rest — no ghost workers may keep
    // writing into a file whose plan is about to be wiped and
    // re-issued (P1-1, M1-c1 R2). Cursors already flushed stay on
    // disk; that is the resume story, not a hazard.
    let mut session_written: u64 = 0;
    let mut last_etag: Option<String> = None;
    let mut first_failure: Option<(ApiError, bool)> = None;
    let mut cancelled = false;
    let mut remaining = handles.into_iter();
    for mut h in remaining.by_ref() {
        // biased: a cancellation ping between worker completions is
        // honored before awaiting the next worker.
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Cooperative stop: no ghost workers may outlive the
                // caller (spawned tasks survive a dropped joiner —
                // they hold file handles and connections into a file
                // whose ownership is about to change). Cursors already
                // flushed are the resume story.
                tracing::debug!(url = %url, "segmented download cancelled");
                None
            }
            // `&mut h`: on the cancel branch the handle must stay
            // owned HERE — dropping it would detach the worker
            // (drop ≠ abort), leaking exactly one ghost (M2-b R2
            // P0-1).
            r = &mut h => Some(
                r.map_err(|e| fatal(ApiError::Network(format!("segment worker panicked: {e}"))))
            ),
        };
        let Some(r) = outcome else {
            h.abort();
            let _ = h.await;
            cancelled = true;
            break;
        };
        match r {
            Ok(Ok((written, etag))) => {
                session_written += written;
                if etag.is_some() {
                    last_etag = etag;
                }
            }
            Ok(Err((e, restart))) => {
                tracing::warn!(error = %e, "segment worker failed");
                first_failure = Some((e, restart));
                break;
            }
            Err(e) => {
                // Worker panic: abort every unjoined worker before
                // surfacing — dropping handles detaches them (P1-4).
                for h in remaining.by_ref() {
                    h.abort();
                    let _ = h.await;
                }
                return Err(e);
            }
        }
    }
    for h in remaining {
        h.abort();
        let _ = h.await;
    }
    if cancelled {
        return Err(fatal(ApiError::Cancelled));
    }
    if let Some(failure) = first_failure {
        return Err(failure);
    }

    // --- completion proof ---------------------------------------------
    // Reload from the store: every segment must report done == len.
    // This is the only place "completed" is decided, and it is decided
    // from persisted state, not from worker in-memory claims.
    let final_state = store
        .get_task(url, sink)
        .await
        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?
        .ok_or_else(|| fatal(ApiError::Storage("task row vanished mid-download".into())))?;
    let done_now: u64 = final_state.segments.iter().map(|s| s.done).sum();
    if done_now != total || !final_state.segments.iter().all(|s| s.is_complete()) {
        return Err(fatal(ApiError::Network(format!(
            "incomplete after workers exited: {done_now} of {total} bytes confirmed"
        ))));
    }

    // fsync once per session; per-worker handles were already flushed.
    // NOTE (P1-4, M1-c1 R2): this guards kill -9, not power loss —
    // SQLite WAL commits and file page writeback are unordered across
    // a crash, so a cursor can theoretically lead the disk bytes. The
    // sink-length check above downgrades that window from silent
    // corruption to a visible error; a sync-before-cursor protocol
    // would cost one fsync per 1 MiB and is deliberately not paid.
    {
        let f = tokio::fs::File::open(sink)
            .await
            .map_err(|e| fatal(ApiError::Io(format!("open {}: {e}", sink.display()))))?;
        f.sync_all()
            .await
            .map_err(|e| fatal(ApiError::Io(format!("sync {}: {e}", sink.display()))))?;
    }

    // Final events + bookkeeping. `set_validator` BEFORE delete is moot
    // for the row, but keeps the code honest if delete becomes soft.
    store
        .set_validator(task_id, last_etag.as_deref())
        .await
        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;
    store
        .delete_task(task_id)
        .await
        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;

    progress.on_progress(&DownloadProgress {
        bytes_done: total,
        total: Some(total),
    });

    Ok(DownloadOutcome {
        bytes_written: session_written,
        total_bytes: Some(total),
        completed: true,
        final_url: url.clone(),
        final_validator: last_etag.map(IfRangeValidator::StrongEtag),
    })
}

/// Fresh plan + persisted rows for a task starting from zero. Also the
/// wipe path: `set_validator` after `replace_segments` clears a stale
/// stored etag (the upsert's COALESCE would otherwise keep the
/// validator of the DEAD version — P2-2, M1-c1 R2).
async fn replan(
    store: &Store,
    url: &str,
    sink: &std::path::Path,
    total: u64,
    validator_wire: &Option<String>,
    cfg: &SegmentConfig,
) -> Result<(TaskId, Vec<SegmentState>), ApiError> {
    let ranges = plan_ranges(total, cfg);
    let task_id = store
        .upsert_task(url, sink, Some(total), validator_wire.as_deref())
        .await
        .map_err(|e| ApiError::Storage(e.to_string()))?;
    store
        .replace_segments(task_id, &ranges)
        .await
        .map_err(|e| ApiError::Storage(e.to_string()))?;
    store
        .set_validator(task_id, validator_wire.as_deref())
        .await
        .map_err(|e| ApiError::Storage(e.to_string()))?;
    let segments = ranges
        .iter()
        .enumerate()
        .map(|(idx, &(start, end))| SegmentState {
            idx: idx as u32,
            start,
            end,
            done: 0,
        })
        .collect();
    Ok((task_id, segments))
}

/// Everything a segment worker needs besides its own `SegmentState`.
/// Cloned per worker: every field is cheap (Arc handles / small owned
/// types), so no shared mutable state beyond the atomic counter.
#[derive(Clone)]
struct SegmentCtx {
    client: HttpsClient,
    max_redirects: usize,
    /// Whole-resource total (planned cover of all segments) — the
    /// Content-Range total must match THIS, not the segment's own end.
    total: u64,
    url: String,
    sink: std::path::PathBuf,
    task_id: TaskId,
    validator: Option<String>,
    store: Store,
    done_counter: Arc<AtomicU64>,
    progress: peregrine_api::download::SharedProgressSink,
}

/// Fetch one segment's remaining bytes into place. Returns
/// (bytes written this session, served ETag if any); the error's
/// bool is "the resource changed — restart the task from zero".
///
/// Each worker opens its OWN file handle and seeks once to its
/// frontier: handles are cheap, and independent cursors keep workers
/// from racing on a shared offset.
async fn run_segment(
    ctx: SegmentCtx,
    seg: SegmentState,
) -> Result<(u64, Option<String>), (ApiError, bool)> {
    let fatal = |e: ApiError| (e, false);
    let restart = |e: ApiError| (e, true);
    let SegmentCtx {
        client,
        max_redirects,
        total,
        url,
        ref sink,
        task_id,
        validator,
        store,
        done_counter,
        progress,
    } = ctx;
    let frontier = seg.frontier();
    let want_range = format!("bytes={frontier}-{}", seg.end);
    let start_url = url::Url::parse(&url)
        .map_err(|e| fatal(ApiError::Network(format!("invalid url {url:?}: {e}"))))?;

    let (res, final_url) = fetch_get(
        &client,
        max_redirects,
        &start_url,
        Some(&want_range),
        validator.as_deref(),
    )
    .await
    .map_err(fatal)?;
    let status = res.status();
    match status.as_u16() {
        // Server ignored Range. Two very different causes:
        //  - we vouched for the old bytes with If-Range and the server
        //    rejected it → the resource changed → restart the whole
        //    task from zero (P0-2, one level up);
        //  - no validator was sent and the server just ignores Range
        //    → gluing at `frontier` would corrupt, restarting inside
        //    a worker would double-download → structured downgrade
        //    signal; the auto-router restarts single-stream.
        200 if validator.is_some() => {
            return Err(restart(ApiError::Network(format!(
                "resource changed: server rejected If-Range for {final_url}"
            ))));
        }
        200 => {
            return Err(fatal(ApiError::SingleStreamRequired {
                reason: format!("server ignored Range ({want_range}) for {final_url}"),
            }));
        }
        206 => {}
        other if status.is_success() => {
            return Err(fatal(ApiError::Network(format!(
                "unexpected status {other} answering {want_range}: {final_url}"
            ))));
        }
        other => {
            return Err(fatal(ApiError::Http {
                status: other,
                url: final_url.to_string(),
            }));
        }
    }

    // B20 two-ended validation: we asked for a CLOSED range; the 206
    // must echo exactly [frontier, seg.end].
    let headers = res.headers().clone();
    let (start, end, total_hint) = HttpEngine::parse_content_range(&headers).ok_or_else(|| {
        // Broken-CDN shape (P2-2, M1-c2 R2): a 206 we cannot parse is
        // the same betrayal family — single-stream (which sends no
        // Range at all) would succeed here.
        fatal(ApiError::SingleStreamRequired {
            reason: format!("206 without parseable Content-Range: {final_url}"),
        })
    })?;
    if start != frontier || end != seg.end {
        return Err(fatal(ApiError::Network(format!(
            "server answered 206 for bytes {start}-{end}, asked {want_range} — \
             refusing mismatched bytes"
        ))));
    }
    if let Some(t) = total_hint.filter(|t| *t != total) {
        return Err(fatal(ApiError::Network(format!(
            "resource total changed mid-download: Content-Range says {t}, planned for {total}"
        ))));
    }
    let served_etag = headers
        .get(hyper::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // Same guard the single-stream path has (P1-3, M1-c1 R2): a
    // server that ignores If-Range (honors only Range) answers 206
    // even for a changed resource — the served ETag betrays it.
    if let (Some(sent), Some(served)) = (validator.as_deref(), served_etag.as_deref())
        && sent != served
    {
        return Err(restart(ApiError::Network(format!(
            "resource changed mid-resume: If-Range validator {sent:?} no \
             longer matches served etag {served:?} — refusing to glue"
        ))));
    }

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(sink)
        .await
        .map_err(|e| fatal(ApiError::Io(format!("open {}: {e}", sink.display()))))?;
    file.seek(std::io::SeekFrom::Start(frontier))
        .await
        .map_err(|e| fatal(ApiError::Io(format!("seek to {frontier}: {e}"))))?;

    let mut body = res.into_body();
    let mut written: u64 = 0; // this session, this segment
    let mut since_persist: u64 = 0; // bytes since last cursor flush
    let mut since_progress: u64 = 0;

    loop {
        let frame = tokio::time::timeout(STALL_TIMEOUT, body.frame())
            .await
            .map_err(|_| {
                fatal(ApiError::Network(format!(
                    "segment {} stalled: no data for {}s ({} of {} bytes)",
                    seg.idx,
                    STALL_TIMEOUT.as_secs(),
                    seg.done + written,
                    seg.len()
                )))
            })?
            .transpose()
            .map_err(|e| fatal(ApiError::Network(format!("body read {final_url}: {e}"))))?;

        let Some(frame) = frame else { break }; // clean EOF

        if let Some(chunk) = frame.data_ref().filter(|c| !c.is_empty()) {
            let n = chunk.len() as u64;
            // Over-serve guard (P1-2, M1-c1 R2): a body longer than
            // the requested range must NEVER spill into the next
            // segment's territory — truncate-hard, not silently.
            // Downgrade-able (P1-3, M1-c2 R2): a 206 that echoes the
            // right Content-Range but streams past it is the same
            // "stopped honoring Range" betrayal — retry-segmented
            // hits the same wall, single-stream succeeds.
            if written + n > seg.len() {
                return Err(fatal(ApiError::SingleStreamRequired {
                    reason: format!(
                        "segment {} over-serve: {} bytes served for a {}-byte range — \
                         refusing to spill into the next segment",
                        seg.idx,
                        written + n,
                        seg.len()
                    ),
                }));
            }
            file.write_all(chunk)
                .await
                .map_err(|e| fatal(ApiError::Io(format!("write {}: {e}", sink.display()))))?;
            written += n;
            since_persist += n;
            since_progress += n;

            if since_persist >= CURSOR_PERSIST_BYTES {
                store
                    .update_cursor(task_id, seg.idx, seg.done + written)
                    .await
                    .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;
                since_persist = 0;
            }
            if since_progress >= PROGRESS_EVERY_BYTES {
                let now =
                    done_counter.fetch_add(since_progress, Ordering::Relaxed) + since_progress;
                progress.on_progress(&DownloadProgress {
                    bytes_done: now,
                    // File-level total, ALWAYS (P1-5, M1-c1 R2): bytes_done
                    // is cumulative for the file, so the total must be too
                    // — never the segment's own end.
                    total: Some(total),
                });
                since_progress = 0;
            }
        }
    }

    file.flush()
        .await
        .map_err(|e| fatal(ApiError::Io(format!("flush {}: {e}", sink.display()))))?;

    // Short-read detection: within one session a segment must fill to
    // its end — anything less is a truncated transfer.
    if seg.done + written < seg.len() {
        return Err(fatal(ApiError::Network(format!(
            "segment {} short read: {} of {} bytes from {final_url}",
            seg.idx,
            seg.done + written,
            seg.len()
        ))));
    }

    // Final cursor + progress for this segment.
    store
        .update_cursor(task_id, seg.idx, seg.done + written)
        .await
        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;
    if since_progress > 0 {
        let now = done_counter.fetch_add(since_progress, Ordering::Relaxed) + since_progress;
        progress.on_progress(&DownloadProgress {
            bytes_done: now,
            total: Some(total),
        });
    }

    Ok((written, served_etag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SegmentConfig {
        SegmentConfig::new(1024, 8)
    }

    fn lens(ranges: &[(u64, u64)]) -> Vec<u64> {
        ranges.iter().map(|&(s, e)| e - s + 1).sum::<u64>();
        ranges.iter().map(|&(s, e)| e - s + 1).collect()
    }

    #[test]
    fn plans_exact_partition() {
        for (total, k) in [
            (10_000u64, 8usize),
            (6_000, 8),
            (1, 8),
            (8_191, 8),
            (8_192, 8),
        ] {
            let ranges = plan_ranges(total, &cfg());
            // Partition: contiguous, no gaps/overlap, exact cover.
            assert_eq!(ranges.first().unwrap().0, 0, "starts at 0 (total {total})");
            assert_eq!(
                ranges.last().unwrap().1,
                total - 1,
                "ends at total-1 (total {total})"
            );
            for w in ranges.windows(2) {
                assert_eq!(w[0].1 + 1, w[1].0, "contiguous (total {total})");
            }
            let sum: u64 = lens(&ranges).iter().sum();
            assert_eq!(sum, total, "exact cover (total {total})");
            assert!(ranges.len() <= k, "K respected (total {total})");
        }
    }

    #[test]
    fn merges_tiny_pieces() {
        // 6 KiB with 1 KiB floor → 6 segments allowed, but
        // max_concurrency clamps the K; with K=2 the pieces are 3 KiB.
        let c = SegmentConfig::new(1024, 2);
        let ranges = plan_ranges(6_144, &c);
        assert_eq!(ranges.len(), 2);
        assert_eq!(lens(&ranges), vec![3_072, 3_072]);
    }

    #[test]
    fn min_segment_floor_reduces_k() {
        // 10 MiB with a 5 MiB floor → only 2 segments.
        let c = SegmentConfig::new(5 * 1024 * 1024, 8);
        let ranges = plan_ranges(10 * 1024 * 1024, &c);
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn zero_total_is_empty_plan() {
        assert!(plan_ranges(0, &cfg()).is_empty());
    }

    #[test]
    fn config_clamps() {
        let c = SegmentConfig::new(0, 999);
        assert_eq!(c.min_segment_bytes, 1);
        assert_eq!(c.max_concurrency, HARD_CONCURRENCY_CEILING);
        let c = SegmentConfig::new(1, 0);
        assert_eq!(c.max_concurrency, 1);
    }

    #[test]
    fn adapts_api_config_even_when_unsanitized() {
        use peregrine_api::SegmentConfig as ApiCfg;
        let c = SegmentConfig::from(ApiCfg {
            max_conns: 999,
            min_segment: 0,
        });
        assert_eq!(c.max_concurrency, HARD_CONCURRENCY_CEILING);
        assert_eq!(c.min_segment_bytes, 1);
        let c = SegmentConfig::from(ApiCfg::default());
        assert_eq!(c.max_concurrency, 8);
        assert_eq!(c.min_segment_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn remainder_spread_over_first_segments() {
        let ranges = plan_ranges(10, &SegmentConfig::new(1, 3));
        assert_eq!(lens(&ranges), vec![4, 3, 3]);
    }
}
