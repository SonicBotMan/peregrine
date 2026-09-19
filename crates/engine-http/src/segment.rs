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
use futures::StreamExt;
use http_body_util::BodyExt;
use peregrine_api::download::{DownloadJob, DownloadOutcome, IfRangeValidator};
use peregrine_api::{ApiError, DownloadProgress};
use peregrine_storage::{SegmentState, Store, TaskId};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;

/// Rebalance cadence (roadmap item 1): how often the driver looks for
/// an idle slot + a fat remaining range to split. 1s — the probe is
/// in-memory only, the cost is a scan of ≤32 entries.
const REBALANCE_TICK: Duration = Duration::from_secs(1);

/// One live segment as the swarm sees it: the (stealable) end, the
/// worker's in-memory frontier, and whether the tail was ever split.
#[derive(Default)]
pub(crate) struct SegLive {
    /// Current end (starts at the plan's end; shrinks on a split).
    pub(crate) end: std::sync::atomic::AtomicU64,
    /// The worker's absolute write frontier (start + done + written),
    /// updated per chunk — the split point is always ≥ this, so a
    /// split can never orphan written bytes into the tail.
    pub(crate) frontier: std::sync::atomic::AtomicU64,
    /// Set once the tail has been stolen: the worker then treats
    /// "body continues past my (new) end" as truncation, not betrayal.
    pub(crate) shrunk: std::sync::atomic::AtomicBool,
}

/// In-memory view of the live segment set: idx → state. Sits beside
/// the SQLite rows (the durable truth) so workers read their end
/// without a database round-trip per chunk. Split writes go to BOTH
/// — the store transaction first, then the table.
#[derive(Clone, Default)]
pub(crate) struct SegTable(Arc<std::sync::Mutex<std::collections::BTreeMap<u32, Arc<SegLive>>>>);

impl SegTable {
    pub(crate) fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(
            std::collections::BTreeMap::new(),
        )))
    }

    pub(crate) fn insert(&self, idx: u32, live: Arc<SegLive>) {
        self.0.lock().unwrap().insert(idx, live);
    }

    pub(crate) fn remove(&self, idx: u32) {
        self.0.lock().unwrap().remove(&idx);
    }

    /// (idx, live) pairs with bytes still to fetch, longest remaining
    /// first — the steal candidate order.
    pub(crate) fn longest_remaining(&self) -> Vec<(u32, Arc<SegLive>)> {
        let map = self.0.lock().unwrap();
        let mut v: Vec<(u32, Arc<SegLive>)> = map
            .iter()
            .filter(|(_, l)| {
                let end = l.end.load(std::sync::atomic::Ordering::Relaxed);
                let f = l.frontier.load(std::sync::atomic::Ordering::Relaxed);
                f <= end // frontier past end = worker is finishing up
            })
            .map(|(i, l)| (*i, l.clone()))
            .collect();
        v.sort_by_key(|(_, l)| {
            let end = l.end.load(std::sync::atomic::Ordering::Relaxed);
            let f = l.frontier.load(std::sync::atomic::Ordering::Relaxed);
            std::cmp::Reverse(end.saturating_sub(f))
        });
        v
    }
}

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
/// `Store` for cursor persistence. On success the plan rows are kept
/// as the completed-state telemetry + re-add resume memory; cleanup
/// is owned by task remove (see the completion bookkeeping below).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_segmented_download(
    client: &HttpsClient,
    max_redirects: usize,
    job: DownloadJob,
    cfg: &SegmentConfig,
    store: &Store,
    progress: &peregrine_api::download::SharedProgressSink,
    cancel: tokio_util::sync::CancellationToken,
    budget: &peregrine_api::budget::BudgetChain,
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
            budget,
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
    budget: &peregrine_api::budget::BudgetChain,
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
    //     differ (two generations, both visible).
    // NEITHER side having a validator is NOT staleness (M3-b1 smoke
    // P0: big static mirrors like TUNA send no ETag; replanning on
    // every resume turned "resume" into "start over" for them, and
    // the resulting done-counter reset fought the store's MAX()
    // progress clamp into a frozen received_bytes). Mid-flight
    // betrayal is already covered one level down: a changed resource
    // answers a cursor's Range with 200, which routes to the
    // single-stream downgrade — trust the cursors, let the
    // per-segment guards catch the rare real change.
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
    // Declare the session base FIRST (M3-b1 smoke P0): sinks that
    // track a cumulative column of their own (the scheduler's row)
    // re-base this session's readings onto it — a paused task's
    // cursors lag the row's reading by the tail quantum, and raw
    // absolutes would freeze that column behind its monotone max().
    progress.on_session_base(initial_done);
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
        table: SegTable::new(),
        min_split_bytes: cfg.min_segment_bytes,
        fetch_url: job.fetch_url().to_string(),
        sink: sink.clone(),
        task_id,
        validator: if_range,
        store: store.clone(),
        done_counter: done_counter.clone(),
        progress: progress.clone(),
        budget: budget.clone(),
    };
    let table = ctx.table.clone();
    let permits = Arc::new(Semaphore::new(cfg.max_concurrency));
    let mut handles = futures::stream::FuturesUnordered::new();
    for seg in &segments {
        if seg.is_complete() {
            continue;
        }
        let live = Arc::new(SegLive {
            end: std::sync::atomic::AtomicU64::new(seg.end),
            frontier: std::sync::atomic::AtomicU64::new(seg.start + seg.done),
            shrunk: std::sync::atomic::AtomicBool::new(false),
        });
        table.insert(seg.idx, live.clone());
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| fatal(ApiError::Network("segment pool closed".into())))?;
        let ctx = ctx.clone();
        let seg = *seg;
        handles.push(tokio::spawn(async move {
            let _permit = permit; // held for the worker's lifetime
            run_segment(ctx, seg, live).await
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
    let mut cancel_fired = false;
    // Dynamic rebalancing tick (roadmap item 1): while workers run,
    // an idle slot steals the fattest remaining tail. `FuturesUnordered`
    // lets splits JOIN the same pool mid-download — the old fixed
    // Vec could only drain, never grow.
    let mut tick = tokio::time::interval(REBALANCE_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    'swarm: loop {
        let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            // Cooperative stop: no ghost workers may outlive the
            // caller (spawned tasks survive a dropped joiner —
            // they hold file handles and connections into a file
            // whose ownership is about to change). Cursors already
            // flushed are the resume story.
            tracing::debug!(url = %url, "segmented download cancelled");
            cancel_fired = true;
            None
        }
        _ = tick.tick() => {
            // Idle slot + fat remaining tail → split & spawn. Failures
            // here are best-effort: a refused split just means the
            // swarm keeps its current shape.
            if let Err(e) =
                maybe_split(&ctx, store, &permits, &mut handles, cancel).await
            {
                tracing::debug!(error = %e, "rebalance pass skipped");
            }
            continue 'swarm;
        }
        r = handles.next() => r.map(
            |r| r.map_err(|e| fatal(ApiError::Network(format!("segment worker panicked: {e}")))),
        ),
        };
        // `None` is ambiguous: cancel fired, or the pool drained
        // (every worker finished). `cancel_fired` disambiguates —
        // treating a drained pool as cancellation would fail every
        // happy download with `Err(Cancelled)`.
        let Some(r) = outcome else {
            if cancel_fired {
                for h in handles.iter_mut() {
                    h.abort();
                }
                while handles.next().await.is_some() {}
                cancelled = true;
                break;
            }
            break; // pool drained: every segment completed
        };
        match r {
            Ok(Ok((written, etag, seg_idx))) => {
                session_written += written;
                if etag.is_some() {
                    last_etag = etag;
                }
                // The segment is done (or truncated to its stolen
                // end) — drop it from the live set so it cannot be
                // picked as a steal victim again.
                ctx.table.remove(seg_idx);
            }
            Ok(Err((e, restart))) => {
                tracing::warn!(error = %e, "segment worker failed");
                first_failure = Some((e, restart));
                break;
            }
            Err(e) => {
                // Worker panic: abort every unjoined worker before
                // surfacing — dropping handles detaches them (P1-4).
                for h in handles.iter_mut() {
                    h.abort();
                }
                while handles.next().await.is_some() {}
                return Err(e);
            }
        }
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

    // Final events + bookkeeping. The plan rows are DELIBERATELY
    // KEPT after completion (GUI-verify R1 backlog → #31):
    // (a) the completed-state `/tasks/{id}/segments` telemetry
    //     reads them — deleting here emptied the GUI panel the
    //     moment a task finished;
    // (b) a same-(url, sink) re-add resumes onto the done rows,
    //     skips every segment, reports base == total (seed-lift)
    //     and completes instantly — the row IS the "already
    //     downloaded" memory;
    // (c) cleanup has an owner: task remove purges engine rows on
    //     every path (scheduler `purge`, B31 + storage cascade).
    // Stale rows are safe: the resume sink-length check replans
    // if the file shrank/vanished. (R2 P1-1 note: the etag-change
    // replan guard is NOT reachable on the production re-add path —
    // Route 1 short-circuits before probe, so `last_etag` is None
    // there; the two-ended-206 + total guards carry the defense.
    // The `or(stored)` below keeps the row's validator from being
    // erased by the zero-worker instant completion.)
    store
        .set_validator(
            task_id,
            last_etag.as_deref().or(final_state.etag.as_deref()),
        )
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
        replayed_from_zero: false,
    })
}

/// The swarm's join-handle pool: split workers join mid-download.
type HandlePool = futures::stream::FuturesUnordered<
    tokio::task::JoinHandle<Result<(u64, Option<String>, u32), (ApiError, bool)>>,
>;

/// Rebalance pass (roadmap item 1, idle-slot flavour): with a free
/// slot and a segment whose remaining range is ≥ 2× the split floor,
/// steal its TAIL half. The split point is `(frontier + end) / 2` —
/// never before the worker's in-memory frontier, so no written byte
/// falls into the stolen range. The store transaction lands first
/// (crash → consistent plan), then the in-memory table flips, then
/// the new worker spawns. Speed-driven stealing (splitting WITHOUT an
/// idle slot by racing a parked worker) is a deliberate non-goal for
/// this pass — it needs cooperative worker pauses.
#[allow(clippy::too_many_arguments)]
async fn maybe_split(
    ctx: &SegmentCtx,
    store: &Store,
    permits: &Arc<Semaphore>,
    handles: &mut HandlePool,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), ApiError> {
    // (signature kept by reference; the caller passes the Arcs it owns)
    if cancel.is_cancelled() {
        return Ok(());
    }
    if permits.available_permits() == 0 {
        return Ok(()); // no idle slot — nothing to run the stolen tail
    }
    let floor = ctx.min_split_bytes;
    let candidates = ctx.table.longest_remaining();
    for (idx, live) in candidates {
        let end = live.end.load(std::sync::atomic::Ordering::Relaxed);
        let frontier = live.frontier.load(std::sync::atomic::Ordering::Relaxed);
        let remaining = end.saturating_sub(frontier.saturating_sub(1));
        if remaining < floor * 2 {
            break; // sorted longest-first — nothing fatter follows
        }
        let at = frontier + remaining / 2;
        if at >= end {
            continue;
        }
        // Durable first: crash right after the tx leaves a consistent
        // plan (shrunken head + tail segment at done=0).
        let new_idx = store
            .split_segment(ctx.task_id, idx, at)
            .await
            .map_err(|e| ApiError::Storage(e.to_string()))?;
        let old_end = end;
        live.end.store(at, std::sync::atomic::Ordering::Relaxed);
        live.shrunk
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let new_live = Arc::new(SegLive {
            end: std::sync::atomic::AtomicU64::new(old_end),
            frontier: std::sync::atomic::AtomicU64::new(at + 1),
            shrunk: std::sync::atomic::AtomicBool::new(false),
        });
        ctx.table.insert(new_idx, new_live.clone());
        let new_seg = SegmentState {
            idx: new_idx,
            start: at + 1,
            end: old_end,
            done: 0,
        };
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ApiError::Network("segment pool closed".into()))?;
        let worker_ctx = ctx.clone();
        tracing::info!(
            segment = idx,
            new_segment = new_idx,
            at,
            "rebalance: idle slot stole the tail of the fattest remaining segment"
        );
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            run_segment(worker_ctx, new_seg, new_live).await
        }));
        // One split per pass — the next tick re-evaluates with fresh
        // frontiers. Bursts are unnecessary: a 1s cadence outruns any
        // real network.
        return Ok(());
    }
    Ok(())
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
    /// Live segment table (dynamic rebalancing — see SegTable).
    table: SegTable,
    /// Split floor: a split tail is never smaller than this (same
    /// floor as the planner — a tail worth stealing is worth a dial).
    min_split_bytes: u64,
    /// Whole-resource total (planned cover of all segments) — the
    /// Content-Range total must match THIS, not the segment's own end.
    total: u64,
    /// Where workers actually dial (probe-chosen mirror, or the
    /// caller's URL). Storage keys ride on `task_id`, so the ctx
    /// never needs the URL itself.
    fetch_url: String,
    sink: std::path::PathBuf,
    task_id: TaskId,
    validator: Option<String>,
    store: Store,
    done_counter: Arc<AtomicU64>,
    progress: peregrine_api::download::SharedProgressSink,
    /// Byte budget (M3-b): per-task × global chain shared by every
    /// worker of this task. Cheap to clone (two Arcs).
    budget: peregrine_api::budget::BudgetChain,
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
    live: Arc<SegLive>,
) -> Result<(u64, Option<String>, u32), (ApiError, bool)> {
    let fatal = |e: ApiError| (e, false);
    let restart = |e: ApiError| (e, true);
    let SegmentCtx {
        client,
        max_redirects,
        total,
        fetch_url,
        ref sink,
        task_id,
        validator,
        store,
        done_counter,
        progress,
        budget,
        table: _,
        min_split_bytes: _,
    } = ctx;
    // The LIVE end (not the plan snapshot): a rebalance split may
    // have shrunk this segment before the worker even dialed.
    let cur_end = live.end.load(std::sync::atomic::Ordering::Relaxed);
    let frontier = seg.frontier().min(cur_end);
    let want_range = format!("bytes={frontier}-{cur_end}");
    let start_url = url::Url::parse(&fetch_url)
        .map_err(|e| fatal(ApiError::Network(format!("invalid url {fetch_url:?}: {e}"))))?;

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
        // 416 while resuming: our range starts past the resource's
        // end — the remote shrank/changed under the stored cursor.
        // Same betrayal family as a rejected If-Range: retry from
        // zero (the restart loop replans), never surface as a bare
        // HTTP error the auto-router cannot reason about.
        416 => {
            return Err(restart(ApiError::Network(format!(
                "416 for {want_range} at {final_url}: stored range is past the resource end — resource changed"
            ))));
        }
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
    if start != frontier || end != cur_end {
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
    // Progress has a BYTES threshold (don't spam events at LAN speed)
    // and a TIME floor (don't go silent at throttled speed): with a
    // 256 KiB threshold and a per-worker share of ~21 KiB/s under a
    // 128 KiB/s task limit, a pure byte threshold means one event
    // every ~12 s — a live view reads that as dead. Either-or wins
    // in both regimes (M3-b1 smoke finding).
    let mut last_progress = tokio::time::Instant::now();

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
            // The LIVE end (roadmap item 1): a rebalance split may
            // have shrunk this segment mid-transfer.
            let cur_end = live.end.load(std::sync::atomic::Ordering::Relaxed);
            let cur_len = cur_end.saturating_sub(seg.start) + 1 - seg.done;
            // Over-serve guard (P1-2, M1-c1 R2): a body longer than
            // the range must NEVER spill into the next segment's
            // territory. Two reasons a body can overrun:
            //  * our own rebalance stole the tail while this response
            //    was in flight (`shrunk`) → truncate at cur_len and
            //    finish — the stolen range belongs to another worker;
            //  * the server "stopped honoring Range" → betrayal,
            //    structured downgrade (P1-3) — single-stream succeeds.
            if written + n > cur_len {
                let shrunk = live.shrunk.load(std::sync::atomic::Ordering::Relaxed);
                if shrunk {
                    let keep = (cur_len - written) as usize;
                    let keep = keep.min(n as usize);
                    // write the prefix, then treat as clean EOF below
                    // (fall through with a trimmed chunk).
                    let head = &chunk[..keep];
                    // Delegated to the normal write path by trimming:
                    // rewrite chunk as head and skip the loop-end EOF
                    // handling — simplest is to write here and break.
                    budget.acquire(keep as u64).await;
                    file.write_all(head).await.map_err(|e| {
                        fatal(ApiError::Io(format!("write {}: {e}", sink.display())))
                    })?;
                    let t = keep as u64;
                    written += t;
                    since_progress += t;
                    live.frontier.store(
                        seg.start + seg.done + written,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    break;
                }
                return Err(fatal(ApiError::SingleStreamRequired {
                    reason: format!(
                        "segment {} over-serve: {} bytes served for a {}-byte range — \
                         refusing to spill into the next segment",
                        seg.idx,
                        written + n,
                        cur_len
                    ),
                }));
            }
            // Slice a frame LARGER than one second's budget into
            // cap-sized writes (see run_download: one acquire for a
            // multi-MB hyper frame parks for seconds with zero
            // progress). Cancellation is the pool supervisor's
            // abort() — it kills a parked or slicing worker equally,
            // and tokens only leave the bucket after a completed
            // park, so a killed slice debits nothing.
            let hint = budget.slice_hint();
            let mut rest = chunk.as_ref();
            while !rest.is_empty() {
                let take = (rest.len() as u64).min(hint) as usize;
                budget.acquire(take as u64).await;
                file.write_all(&rest[..take])
                    .await
                    .map_err(|e| fatal(ApiError::Io(format!("write {}: {e}", sink.display()))))?;
                let t = take as u64;
                written += t;
                since_persist += t;
                since_progress += t;
                // Frontier for the rebalancer (roadmap item 1): the
                // absolute offset one past the last written byte —
                // split points are derived from it, so it must move
                // with EVERY write, not just the 1 MiB cursor flushes.
                live.frontier.store(
                    seg.start + seg.done + written,
                    std::sync::atomic::Ordering::Relaxed,
                );

                if since_persist >= CURSOR_PERSIST_BYTES {
                    store
                        .update_cursor(task_id, seg.idx, seg.done + written)
                        .await
                        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;
                    since_persist = 0;
                }
                if since_progress >= PROGRESS_EVERY_BYTES
                    || (since_progress > 0 && last_progress.elapsed() >= Duration::from_secs(1))
                {
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
                    last_progress = tokio::time::Instant::now();
                }
                rest = &rest[take..];
            }
        }
    }

    file.flush()
        .await
        .map_err(|e| fatal(ApiError::Io(format!("flush {}: {e}", sink.display()))))?;

    // Final cursor, clamped to the LIVE len (a tail-steal in flight
    // may leave our written bytes past the shrunken end — those bytes
    // belong to the new tail worker, which rewrites them idempotently;
    // the cursor must not exceed the persisted plan's len or the
    // completion proof would fail). `MAX()` in SQL keeps it monotone.
    let cur_end = live.end.load(std::sync::atomic::Ordering::Relaxed);
    let final_done = (seg.done + written).min(cur_end - seg.start + 1);
    // Short-read detection: within one session a segment must fill to
    // ITS (possibly shrunken) end — anything less is a truncated
    // transfer. A tail-stolen exit is NOT a short read: the tail has
    // its own worker.
    if final_done < cur_end - seg.start + 1
        && !live.shrunk.load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(fatal(ApiError::Network(format!(
            "segment {} short read: {} of {} bytes from {final_url}",
            seg.idx,
            final_done,
            cur_end - seg.start + 1
        ))));
    }
    store
        .update_cursor(task_id, seg.idx, final_done)
        .await
        .map_err(|e| fatal(ApiError::Storage(e.to_string())))?;
    if since_progress > 0 {
        let now = done_counter.fetch_add(since_progress, Ordering::Relaxed) + since_progress;
        progress.on_progress(&DownloadProgress {
            bytes_done: now,
            total: Some(total),
        });
    }

    Ok((written, served_etag, seg.idx))
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
