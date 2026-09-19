//! The auto-router (M1-c2): the ONE download entry a caller wants.
//!
//! `download_auto` probes, decides single-stream vs segmented, feeds
//! the right engine, and honors one structured downgrade signal:
//! [`ApiError::SingleStreamRequired`] from the segmenter restarts the
//! job as a single stream. Callers never pick an engine by hand.
//!
//! Routing table (first match wins):
//!
//! | state                                            | route        |
//! |--------------------------------------------------|--------------|
//! | store has a live task row for (url, sink)        | segmented    |
//! | `job.resume` present (single-stream partial)     | single       |
//! | probe says no ranges / unknown size / too small  | single       |
//! | probe fails entirely (e.g. HEAD is 405)          | single       |
//! | otherwise                                        | segmented    |
//!
//! Mode stickiness: the two engines leave DIFFERENT partial shapes
//! (single-stream: dense prefix; segmented: sparse full-length file +
//! cursors), and each refuses to glue the other's shape. A store row
//! is therefore treated as the record of "this sink is segmented" —
//! resume follows the mode the partial was written in, never a fresh
//! probe's opinion. The M2 task layer will hold this explicitly; for
//! M1 the store row IS the task record.

use crate::HttpEngine;
use peregrine_api::ProtocolEngine;
use peregrine_api::{
    ApiError, DownloadJob, DownloadOutcome, ProbeInfo, ResumeContext, SharedProgressSink,
};
use peregrine_storage::Store;
use tokio_util::sync::CancellationToken;
impl HttpEngine {
    /// Probe, route, download. See the module docs for the routing
    /// table. `store` is the segment-cursor store; `cfg` the segmenter
    /// tuning. `cancel` threads through to both engines: a cancelled
    /// call leaves a valid partial and returns `ApiError::Cancelled`.
    /// This is the entry M2's task manager will call.
    pub async fn download_auto(
        &self,
        mut job: DownloadJob,
        cfg: &crate::segment::SegmentConfig,
        store: &Store,
        progress: SharedProgressSink,
        cancel: CancellationToken,
        budget: &peregrine_api::budget::BudgetChain,
    ) -> Result<DownloadOutcome, ApiError> {
        // Route 1: a live task row means the sink is a segmented
        // partial — resume in mode, ignoring today's probe (P0-2's
        // per-request staleness checks still apply inside).
        let existing = store
            .get_task(&job.url, &job.sink)
            .await
            .map_err(|e| ApiError::Storage(e.to_string()))?;
        if let Some(row) = existing.clone().filter(|t| t.total.is_some()) {
            tracing::debug!("store row present — resuming segmented");
            // The row is the authority for a total the caller didn't
            // provide; one it DID provide outranks the row (the row
            // may predate a size change — run_attempt's cover check
            // and the workers' total-drift guard then settle it).
            // The row's etag rides out as If-Range on every worker
            // (run_attempt already prefers the stored validator when
            // the probe handed us none).
            if job.expected_total.is_none() {
                job.expected_total = row.total;
            }
            let original = job.url.clone();
            return run_with_downgrade(self, job, original, cfg, store, progress, cancel, budget)
                .await;
        }

        // Route 2: a single-stream partial in the caller's hands.
        if job.resume.is_some() {
            tracing::debug!("resume context present — single stream");
            // B36 read side: a row WITHOUT a total is the
            // single-stream session's validator record (persisted at
            // the previous session's first response — see
            // run_download_impl). Feed it as the resume `If-Range`:
            // an unchanged remote answers 206 and the append
            // proceeds; a CHANGED remote answers 200 and the
            // truncate branch rewrites from zero — instead of the
            // silent mixed-body glue this used to be. A total-bearing
            // row never reaches here (Route 1 took it), and an
            // absent/weak validator degrades to validator-less
            // resume (exactly today's behavior). A caller-provided
            // validator (probe-fresh) outranks the row: the row is
            // at least one session old, and the worst case of
            // either choice is one safe full replay (R2 P2-5).
            if let Some(v) = existing
                .as_ref()
                .and_then(|r| r.etag.as_deref())
                .and_then(peregrine_api::IfRangeValidator::from_wire)
                && let Some(ctx) = job.resume.as_mut()
                && ctx.validator.is_none()
            {
                tracing::debug!(validator = ?v, "resume: row validator -> If-Range");
                ctx.validator = Some(v);
            }
            return self.download(job, progress, cancel, budget).await;
        }

        // Fresh task: probe and decide — with mirror failover
        // (roadmap item 3). A probe failure on the primary tries each
        // mirror in order; the first that probes clean becomes this
        // download's fetch source (`job.fetch_base`). Storage keys
        // stay on the caller's URL, and mid-download source switches
        // never happen (If-Range validators are source-bound).
        let info = match self.probe(&job.url).await {
            Ok(info) => Some(info),
            Err(primary_err) => {
                let mut mirror_probe: Option<(String, ProbeInfo)> = None;
                for m in &job.mirrors {
                    match self.probe(m).await {
                        Ok(info) => {
                            tracing::info!(
                                mirror = %m,
                                primary = %job.url,
                                "primary probe failed — failover to mirror"
                            );
                            mirror_probe = Some((m.clone(), info));
                            break;
                        }
                        Err(me) => {
                            tracing::warn!(mirror = %m, error = %me, "mirror probe failed");
                        }
                    }
                }
                match mirror_probe {
                    Some((m, info)) => {
                        job.fetch_base = Some(info.url.clone());
                        let _ = m;
                        Some(info)
                    }
                    None => {
                        // No mirror either: the historical behavior —
                        // fall through to single-stream and let IT
                        // produce the user-facing error if the URL is
                        // truly dead — same status, better context.
                        tracing::debug!(error = %primary_err, "probe failed (mirrors exhausted) — trying single stream");
                        None
                    }
                }
            }
        };

        let use_segments = info.as_ref().is_some_and(|i| {
            i.accept_ranges && i.content_length.is_some_and(|t| t >= segmented_floor(cfg))
        });

        if use_segments {
            let info = info.expect("use_segments implies Some");
            // The probe's final URL (post-redirect) is where workers
            // should aim; its validator is the If-Range they send.
            // `original` stays the ROW KEY: every read side
            // (scheduler resume_job, Route 1/2 lookups here) queries
            // by the caller's URL, so a validator-only row written
            // under the final URL would be unreachable forever
            // (B36-R2 P2-3 key drift — fixed by threading the
            // original through the downgrade restart).
            let original = job.url.clone();
            if let Some(base) = job.fetch_base.clone() {
                // Mirror-selected (roadmap item 3): the row key STAYS
                // the caller's URL — the mirror is only where bytes
                // come from. Its probe already resolved redirects.
                job.fetch_base = Some(base);
            } else {
                job.url = info.url.clone();
            }
            job.expected_total = info.content_length;
            job.resume = Some(ResumeContext::from_probe(&info, 0));
            tracing::debug!(total = ?info.content_length, "routing: segmented");
            run_with_downgrade(self, job, original, cfg, store, progress, cancel, budget).await
        } else {
            tracing::debug!("routing: single stream");
            self.download(job, progress, cancel, budget).await
        }
    }
}

/// Run the segmented engine; on its structured downgrade signal
/// ([`ApiError::SingleStreamRequired`]) restart the job single-stream
/// from zero. The restart drops the resume context — the partial on
/// disk belongs to a segment plan we no longer trust, and the
/// single-stream engine truncates on a validator-less replay anyway.
/// Same 8-arg shape as `run_download_impl` (allowed there too):
/// the thread-through of store/progress/cancel/budget + the
/// original-URL key leaves no natural grouping worth a struct yet.
#[allow(clippy::too_many_arguments)]
async fn run_with_downgrade(
    engine: &HttpEngine,
    job: DownloadJob,
    original_url: String,
    cfg: &crate::segment::SegmentConfig,
    store: &Store,
    progress: SharedProgressSink,
    cancel: CancellationToken,
    budget: &peregrine_api::budget::BudgetChain,
) -> Result<DownloadOutcome, ApiError> {
    match engine
        .download_segmented(
            job.clone(),
            cfg,
            store,
            progress.clone(),
            cancel.clone(),
            budget,
        )
        .await
    {
        Ok(out) => Ok(out),
        Err(ApiError::SingleStreamRequired { reason }) => {
            // Cancellation beats the downgrade restart (M2-b R2
            // P1-2): restarting destroys all progress (delete_task +
            // single-stream truncate) before the body loop ever sees
            // the token — a pause would silently lose every byte.
            // Re-checked here so the race between the worker's
            // downgrade signal and the user's cancel resolves on the
            // side of keeping the cursors.
            if cancel.is_cancelled() {
                return Err(ApiError::Cancelled);
            }
            tracing::info!(%reason, "segmenter downgraded — restarting single stream");
            // Drop the segment row first (P1-1/P1-2, M1-c2 R2): its
            // cursors describe a plan we no longer trust. Left
            // behind, a half-done row wedges every later resume
            // against the dense partial the restart is about to
            // write ("resume mismatch"), and a complete one makes
            // the next call re-download a finished file. A failed
            // delete only costs that waste, so it is logged, not
            // fatal. A failed READ is propagated: it means the
            // store is broken, and pretending otherwise would hide
            // the wedge we are trying to prevent.
            let row = store
                .get_task(&job.url, &job.sink)
                .await
                .map_err(|e| ApiError::Storage(e.to_string()))?;
            if let Some(row) = row
                && let Err(e) = store.delete_task(row.id).await
            {
                tracing::warn!(error = %e, "failed to drop stale segment row");
            }
            let mut fresh = job;
            fresh.resume = None;
            // Row-key unification (B36-R2 P2-3): the restart keys its
            // validator row on the CALLER's URL, not the probe's
            // final URL, so the next resume finds it. `job.url` still
            // deletes the segment row under the key it was written.
            fresh.url = original_url;
            engine.download(fresh, progress, cancel, budget).await
        }
        Err(e) => Err(e),
    }
}

/// Smallest total worth splitting: at least two segments at the
/// configured floor, else parallelism buys nothing but cursor churn.
fn segmented_floor(cfg: &crate::segment::SegmentConfig) -> u64 {
    cfg.min_segment_bytes.saturating_mul(2)
}
