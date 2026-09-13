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
use peregrine_api::{ApiError, DownloadJob, DownloadOutcome, ResumeContext, SharedProgressSink};
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
        if let Some(row) = existing.filter(|t| t.total.is_some()) {
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
            return run_with_downgrade(self, job, cfg, store, progress, cancel, budget).await;
        }

        // Route 2: a single-stream partial in the caller's hands.
        if job.resume.is_some() {
            tracing::debug!("resume context present — single stream");
            return self.download(job, progress, cancel, budget).await;
        }

        // Fresh task: probe and decide.
        let info = match self.probe(&job.url).await {
            Ok(info) => Some(info),
            Err(e) => {
                // A failed probe is not a failed download: plenty of
                // servers reject HEAD outright (405) but serve GETs
                // fine. Fall through to single-stream and let IT
                // produce the user-facing error if the URL is truly
                // dead — same status, better context.
                tracing::debug!(error = %e, "probe failed — trying single stream");
                None
            }
        };

        let use_segments = info.as_ref().is_some_and(|i| {
            i.accept_ranges && i.content_length.is_some_and(|t| t >= segmented_floor(cfg))
        });

        if use_segments {
            let info = info.expect("use_segments implies Some");
            // The probe's final URL (post-redirect) is where workers
            // should aim; its validator is the If-Range they send.
            job.url = info.url.clone();
            job.expected_total = info.content_length;
            job.resume = Some(ResumeContext::from_probe(&info, 0));
            tracing::debug!(total = ?info.content_length, "routing: segmented");
            run_with_downgrade(self, job, cfg, store, progress, cancel, budget).await
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
async fn run_with_downgrade(
    engine: &HttpEngine,
    job: DownloadJob,
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
