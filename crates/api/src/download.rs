//! Download contract — the M1-b execution phase of the pipeline.
//!
//! `probe` learns what the server claims; `download` moves bytes. A
//! [`DownloadJob`] is a single-engine, single-connection request: URL
//! (post-probe final URL), a sink file, and an optional [`ResumeContext`]
//! describing the partial bytes already on disk from a previous session.
//!
//! The multi-connection segmenter (M1-c, PROPOSAL §5) will EXTEND this
//! contract — bounded segment ranges (`bytes=A-B`), `fallocate` +
//! positioned writes, per-segment validator persistence — rather than
//! use it as-is. The single-stream shape below is deliberately minimal:
//! what one connection needs, nothing more.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed future for [`crate::engine::ProtocolEngine::download`].
pub type DownloadFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// How to validate a resume: is the resource still the one we probed?
///
/// Only a STRONG etag may back `If-Range` (RFC 7233 §3.2): a weak tag
/// makes the server return the full body, which glued onto a partial
/// file corrupts it silently. `Last-Modified` is the standard fallback
/// (1-second granularity — good enough; collisions within one second
/// on the same URL are a non-issue for downloads).
///
/// Strength is enforced by [`ResumeContext::from_probe`]; the enum stays
/// open so MCP clients can supply a hand-trusted validator, but a weak
/// etag placed in [`IfRangeValidator::StrongEtag`] by hand degrades
/// safely: servers MUST ignore a weak If-Range and answer 200 → the
/// engine truncates and replays the full body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IfRangeValidator {
    /// A strong etag (no `W/` prefix — enforced at construction).
    StrongEtag(String),
    /// A `Last-Modified` http-date string.
    LastModified(String),
}

/// What the downloader must know to resume a partial file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumeContext {
    /// Bytes already present in the sink file. The engine issues
    /// `Range: bytes={start_offset}-` and appends.
    pub start_offset: u64,
    /// Validator from the original probe, for `If-Range`.
    pub validator: Option<IfRangeValidator>,
}

impl ResumeContext {
    /// Build a resume context from probe output. `start_offset` is the
    /// verified on-disk length; the validator is picked from the probe
    /// with correct strength rules (strong etag preferred, weak etag
    /// refused, `Last-Modified` as fallback).
    pub fn from_probe(info: &crate::engine::ProbeInfo, start_offset: u64) -> Self {
        let validator = if info.etag_strong {
            info.etag.clone().map(IfRangeValidator::StrongEtag)
        } else {
            info.last_modified
                .clone()
                .map(IfRangeValidator::LastModified)
        };
        Self {
            start_offset,
            validator,
        }
    }
}

/// Progress report pushed to the sink as bytes land.
///
/// Counts are CUMULATIVE for the file (resume offset included), so a UI
/// never needs to know how the work was split across connections.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Total bytes of the file that exist (on disk + in flight),
    /// resume offset included. `None` if the size is unknown.
    pub bytes_done: u64,
    /// Total file size, when known (probe or `Content-Range`).
    pub total: Option<u64>,
}

impl DownloadProgress {
    /// Fraction complete, 0.0–1.0. Unknown size → `None` (indeterminate
    /// spinners, not fake progress).
    pub fn fraction(&self) -> Option<f64> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| self.bytes_done as f64 / t as f64)
    }
}

/// Receives progress updates. Object-safe on purpose: M1-c's segment
/// aggregator, M2's task manager and the GUI event bus all implement
/// this one trait.
pub trait ProgressSink: Send + Sync {
    fn on_progress(&self, progress: &DownloadProgress);

    /// Declares the absolute cumulative base THIS session resumes
    /// from (0 for a fresh download, the store cursors' sum for
    /// segmented resume, the sink offset for single-stream resume).
    /// Engines call it once, before any `on_progress` frame, so a
    /// sink that tracks a DIFFERENT cumulative (e.g. the row's own
    /// `received_bytes`, which may lead the cursors) can re-base
    /// the session's readings onto its own column instead of
    /// freezing it behind a monotone max() (M3-b1 smoke P0).
    /// Default: no-op — a sink that ignores bases keeps the raw
    /// absolute-value semantics.
    fn on_session_base(&self, _base: u64) {}
}

/// No-op sink for callers that don't care (internal downloads, tests).
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn on_progress(&self, _progress: &DownloadProgress) {}
}

/// Shared sink handle — cheap to clone into worker tasks.
pub type SharedProgressSink = Arc<dyn ProgressSink>;

/// Tuning knobs for the multi-connection segmenter (PROPOSAL §5.1,
/// phases 2–3). M1-c1 issues a STATIC even split; dynamic rebalancing
/// (phase 4, slow-segment tail claiming) and adaptive worker counts
/// (phase 5) arrive in M1-c2 and consume the same struct.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SegmentConfig {
    /// Upper bound on concurrent segment connections. PROPOSAL caps
    /// the adaptive range at 32; the static planner uses `min(total /
    /// min_segment, max_conns)` so small files get fewer workers.
    pub max_conns: u32,
    /// Segments smaller than this are not worth a connection
    /// (PROPOSAL suggests ~5 MiB). Also the resume-cursor persistence
    /// granularity upper bound rationale: losing ≤ this much per
    /// segment on kill -9 is acceptable.
    pub min_segment: u64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            max_conns: 8,
            min_segment: 5 * 1024 * 1024,
        }
    }
}

impl SegmentConfig {
    /// Clamp to sane bounds so config typos cannot spawn 10k sockets
    /// or 1-byte segments.
    pub fn sanitized(mut self) -> Self {
        self.max_conns = self.max_conns.clamp(1, 32);
        self.min_segment = self.min_segment.clamp(64 * 1024, 64 * 1024 * 1024);
        self
    }
}

/// A single-connection download request.
#[derive(Debug, Clone)]
pub struct DownloadJob {
    /// Final URL (already followed redirects during probe).
    pub url: String,
    /// Destination file path. Parent directories must already exist.
    pub sink: PathBuf,
    /// Resume an existing partial file; `None` starts from byte 0.
    /// Caveat: a server that ignores `Range` (or a stale `If-Range`)
    /// answers with a full 200 replay — the engine truncates the sink
    /// and rewrites from zero. A mid-replay failure therefore leaves a
    /// SHORTER partial than before the attempt. Protocol-inherent;
    /// callers that care should probe first and check validators.
    pub resume: Option<ResumeContext>,
    /// Expected total size (from probe). Used for short-read detection
    /// and progress totals; `None` when the server never said.
    pub expected_total: Option<u64>,
}

/// Terminal state of a completed download session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadOutcome {
    /// Bytes written THIS session (not counting the resume offset).
    pub bytes_written: u64,
    /// Authoritative total size once the body was fully read
    /// (`Content-Range`/`Content-Length`), else the probe's estimate.
    pub total_bytes: Option<u64>,
    /// Always `true` in an `Ok` outcome: a short body relative to the
    /// announced total is returned as `Err` (see below), never as a
    /// `completed=false` success. The field exists so M2's task state
    /// machine can record "file fully on disk" without re-deriving it.
    pub completed: bool,
    /// URL the bytes actually came from (after any redirects).
    pub final_url: String,
    /// The resource's CURRENT validator as served with this body
    /// (from the final response's ETag / Last-Modified), NOT the one
    /// the caller sent. After a 200-replay the old validator is stale;
    /// persisting this one into the segment table (PROPOSAL §5.2)
    /// avoids a second full replay on the next resume.
    pub final_validator: Option<IfRangeValidator>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ProbeInfo;

    fn probe_fixture() -> ProbeInfo {
        ProbeInfo {
            url: "http://x/f".into(),
            content_length: Some(100),
            accept_ranges: true,
            etag: Some("\"v1\"".into()),
            etag_strong: true,
            last_modified: Some("Mon, 07 Sep 2026 00:00:00 GMT".into()),
            filename: None,
        }
    }

    #[test]
    fn resume_prefers_strong_etag_over_last_modified() {
        let r = ResumeContext::from_probe(&probe_fixture(), 50);
        assert_eq!(r.start_offset, 50);
        assert_eq!(
            r.validator,
            Some(IfRangeValidator::StrongEtag("\"v1\"".into()))
        );
    }

    #[test]
    fn resume_refuses_weak_etag_falls_back_to_last_modified() {
        let mut info = probe_fixture();
        info.etag = Some("W/\"v1\"".into());
        info.etag_strong = false;
        let r = ResumeContext::from_probe(&info, 10);
        assert_eq!(
            r.validator,
            Some(IfRangeValidator::LastModified(
                "Mon, 07 Sep 2026 00:00:00 GMT".into()
            ))
        );
    }

    #[test]
    fn resume_without_any_validator() {
        let mut info = probe_fixture();
        info.etag = None;
        info.etag_strong = false;
        info.last_modified = None;
        let r = ResumeContext::from_probe(&info, 0);
        assert_eq!(r.validator, None);
    }

    #[test]
    fn fraction_handles_unknown_and_zero() {
        assert_eq!(
            DownloadProgress {
                bytes_done: 25,
                total: Some(100)
            }
            .fraction(),
            Some(0.25)
        );
        assert_eq!(
            DownloadProgress {
                bytes_done: 25,
                total: None
            }
            .fraction(),
            None
        );
        assert_eq!(
            DownloadProgress {
                bytes_done: 0,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }
}
