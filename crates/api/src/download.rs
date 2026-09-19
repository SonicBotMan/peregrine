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

impl IfRangeValidator {
    /// Rebuild a validator from its stored wire form (the exact
    /// string sent as `If-Range`: a quoted strong etag or an
    /// IMF-fixdate http-date). B36's read side: the engine persists
    /// the wire form in the task row and reconstructs it on resume.
    ///
    /// Type recovery: etags are quoted (`"v1"`); anything else must
    /// be a syntactically valid RFC 7231 IMF-fixdate or it is
    /// rejected (`None` — validator-less resume: whether the server
    /// then answers 200 (safe replay) or 206 (blind append) is up
    /// to it; we no longer pretend to have a validator we can't
    /// express). Against a NONCONFORMANT but self-consistent server
    /// (obs-date `Last-Modified`, exact string match on If-Range)
    /// the old pass-through accidentally worked as change
    /// detection; the trade is protocol correctness for that corner
    /// — recorded in round-E R3. A weak etag (`W/"…"`) can never
    /// back `If-Range` (RFC 7233 §3.2) → `None`, matching
    /// `from_probe`'s refusal. A malformed quoted string is still
    /// treated as an etag — the worst case is a no-match `If-Range`,
    /// which degrades to the same validator-less resume.
    pub fn from_wire(wire: &str) -> Option<Self> {
        if wire.starts_with("W/") {
            return None;
        }
        if wire.starts_with('"') {
            Some(IfRangeValidator::StrongEtag(wire.to_string()))
        } else if valid_http_date(wire) {
            Some(IfRangeValidator::LastModified(wire.to_string()))
        } else {
            None
        }
    }

    /// The exact string to place in the `If-Range` header.
    pub fn wire(&self) -> &str {
        match self {
            IfRangeValidator::StrongEtag(e) => e.as_str(),
            IfRangeValidator::LastModified(d) => d.as_str(),
        }
    }
}

/// Validate an RFC 7231 IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`)
/// — the ONLY date shape a compliant server sends in `Last-Modified`.
/// Fixed-width field checks (weekday/HH:MM:SS bounds, 01–31 day,
/// real month, 4-digit year, `GMT`), lenient exactly where the RFC
/// is: second `60` (leap second), year `0000` (spec silence). The
/// legacy obs-date shapes (RFC 850 / asctime) are REJECTED — a
/// server emitting those is already nonconformant, and refusing
/// keeps `If-Range` on the safe replay path instead of an
/// unmatchable header.
///
/// Hand-rolled (no `httpdate`/`chrono` dep): the format is 29 fixed
/// bytes with no variable-width fields to lex.
pub fn valid_http_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 29 {
        return false;
    }
    const DAYS: [&[u8]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
    const MONTHS: [&[u8]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let f = s.get(r)?;
        f.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| f.parse().ok())
            .flatten()
    };
    b[3] == b','
        && b[4] == b' '
        && b[7] == b' '
        && b[11] == b' '
        && b[16] == b' '
        && b[19] == b':'
        && b[22] == b':'
        && b[25] == b' '
        && &b[26..29] == b"GMT"
        && DAYS.iter().any(|d| &b[0..3] == *d)
        && (1..=31).contains(&num(5..7).unwrap_or(0))
        && MONTHS.iter().any(|m| &b[8..11] == *m)
        && num(12..16).is_some()
        && num(17..19).is_some_and(|h| h <= 23)
        && num(20..22).is_some_and(|m| m <= 59)
        && num(23..25).is_some_and(|s| s <= 60)
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
    /// refused, `Last-Modified` as fallback — and only when it is a
    /// syntactically valid IMF-fixdate, same rule as `from_wire`).
    pub fn from_probe(info: &crate::engine::ProbeInfo, start_offset: u64) -> Self {
        let validator = if info.etag_strong {
            info.etag.clone().map(IfRangeValidator::StrongEtag)
        } else {
            info.last_modified
                .clone()
                .filter(|d| valid_http_date(d))
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
    /// Final URL (already followed redirects during probe). ALSO the
    /// storage row key — mirrors never take over this identity.
    pub url: String,
    /// Mirror URLs (roadmap item 3): tried in order when the primary
    /// probe fails. The FIRST mirror that probes clean becomes this
    /// download's `fetch_base`; mid-download source switching is
    /// deliberately NOT done — a source swap invalidates If-Range
    /// validators and gluing across sources corrupts. Empty = none.
    pub mirrors: Vec<String>,
    /// Where workers actually fetch from: `None` = `url`, `Some` =
    /// a mirror chosen at probe time. Never used as a storage key.
    pub fetch_base: Option<String>,

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

impl DownloadJob {
    /// The URL network requests should target (mirror-aware); storage
    /// row keys always keep using `url`.
    pub fn fetch_url(&self) -> &str {
        self.fetch_base.as_deref().unwrap_or(&self.url)
    }
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
    /// B34: `true` when the caller asked to resume from an offset
    /// but the server answered the request with a FULL 200 body —
    /// the engine truncated the sink and rewrote from zero, so the
    /// resume offset the caller handed in was discarded. Callers
    /// tracking cumulative bytes must rebase on 0 + `bytes_written`,
    /// NOT `start_offset + bytes_written` (that double-counts the
    /// pre-replay prefix). A fresh (non-resume) download is `false`
    /// by definition — there was nothing to replay over.
    pub replayed_from_zero: bool,
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
    fn from_wire_round_trips_both_variants() {
        // Strong etag: quoted → StrongEtag, wire out unchanged.
        let e = IfRangeValidator::from_wire("\"v1\"").unwrap();
        assert_eq!(e, IfRangeValidator::StrongEtag("\"v1\"".into()));
        assert_eq!(e.wire(), "\"v1\"");
        // http-date: unquoted → LastModified, wire out unchanged.
        let lm = IfRangeValidator::from_wire("Mon, 07 Sep 2026 00:00:00 GMT").unwrap();
        assert_eq!(
            lm,
            IfRangeValidator::LastModified("Mon, 07 Sep 2026 00:00:00 GMT".into())
        );
        assert_eq!(lm.wire(), "Mon, 07 Sep 2026 00:00:00 GMT");
    }

    #[test]
    fn from_wire_refuses_weak_etag() {
        // A weak etag can never back If-Range (RFC 7233 §3.2).
        assert!(IfRangeValidator::from_wire("W/\"v1\"").is_none());
    }

    #[test]
    fn from_wire_rejects_malformed_unquoted_garbage() {
        // B36 residual: a bare unquoted token is neither a quoted
        // etag nor a valid IMF-fixdate — rebuilding it as
        // LastModified would send a garbage If-Range that always
        // misses (pointless full replay while pretending to have a
        // validator). Refused now; the caller degrades to
        // validator-less resume, the same safe replay, honestly.
        assert!(IfRangeValidator::from_wire("v1").is_none());
        assert!(IfRangeValidator::from_wire("").is_none());
    }

    #[test]
    fn valid_http_date_accepts_canonical_imf_fixdate() {
        for d in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Mon, 07 Sep 2026 00:00:00 GMT",
            "Wed, 31 Dec 2100 23:59:60 GMT", // leap second is legal
            "Thu, 01 Jan 1970 00:00:00 GMT",
        ] {
            assert!(valid_http_date(d), "must accept {d}");
            assert!(
                IfRangeValidator::from_wire(d).is_some(),
                "from_wire must accept {d}"
            );
        }
    }

    #[test]
    fn valid_http_date_rejects_malformed_and_legacy_shapes() {
        // Wrong bounds.
        assert!(!valid_http_date("Sun, 06 Nov 1994 24:49:37 GMT")); // hour 24
        assert!(!valid_http_date("Sun, 06 Nov 1994 08:60:37 GMT")); // minute 60
        assert!(!valid_http_date("Sun, 06 Nov 1994 08:49:61 GMT")); // second 61
        assert!(!valid_http_date("Sun, 00 Nov 1994 08:49:37 GMT")); // day 00
        assert!(!valid_http_date("Sun, 32 Nov 1994 08:49:37 GMT")); // day 32
        assert!(!valid_http_date("Sun, 06 Xxx 1994 08:49:37 GMT")); // no month
        // Weekday/name consistency with the calendar is NOT checked
        // (would need leap-year math for no safety gain): "Fri, 06
        // Nov 1994 …" parses fine — header shape, not astronomy.
        assert!(valid_http_date("Fri, 06 Nov 1994 08:49:37 GMT"));
        // Legacy obs-date shapes are rejected on purpose.
        assert!(!valid_http_date("Sunday, 06-Nov-94 08:49:37 GMT")); // RFC 850
        assert!(!valid_http_date("Sun Nov  6 08:49:37 1994")); // asctime
        // Structural damage.
        assert!(!valid_http_date("Sun, 06 Nov 1994 08:49:37 UTC")); // non-GMT
        assert!(!valid_http_date("sun, 06 nov 1994 08:49:37 gmt")); // case
        assert!(!valid_http_date("Sun,  6 Nov 1994 08:49:37 GMT")); // space-padded day
        assert!(!valid_http_date(""));
    }

    #[test]
    fn from_probe_filters_invalid_last_modified() {
        // A server sending a malformed Last-Modified gets no
        // validator at all — same rule as from_wire.
        let mut info = probe_fixture();
        info.etag = None;
        info.etag_strong = false;
        info.last_modified = Some("not a date".into());
        let r = ResumeContext::from_probe(&info, 0);
        assert_eq!(r.validator, None);
        // And a well-formed one still rides through.
        info.last_modified = Some("Mon, 07 Sep 2026 00:00:00 GMT".into());
        let r = ResumeContext::from_probe(&info, 0);
        assert_eq!(
            r.validator,
            Some(IfRangeValidator::LastModified(
                "Mon, 07 Sep 2026 00:00:00 GMT".into()
            ))
        );
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
