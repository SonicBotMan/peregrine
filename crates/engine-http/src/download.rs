//! Single-connection streamed download with resume semantics (M1-b).
//!
//! The contract: bytes land on disk (append for a confirmed resume,
//! truncate-and-rewrite when the server replays the full body), progress
//! is cumulative (resume offset included), and every exit path leaves
//! either a complete file or a VALID PARTIAL file the caller can resume
//! from. Cancellation is cooperative via the CancellationToken: the
//! body loop flushes and leaves a valid partial (drop-cancel still
//! works for non-spawned futures — the writes already issued to the OS
//! stay in the page cache, so even `kill -9` cannot corrupt the prefix.
//!
//! Short reads (server closes before the announced total) are ERRORS,
//! not completions — a "finished" file that is silently truncated is
//! the worst possible outcome for a download manager.
//!
//! The multi-connection segmenter (M1-c, PROPOSAL §5) will EXTEND this
//! primitive — bounded segment ranges (`bytes=A-B`), `fallocate` +
//! positioned writes, per-segment validator persistence — rather than
//! call it as-is. The single-stream shape below is deliberately minimal.

use crate::HttpEngine;
use crate::HttpsClient;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::Request;
use hyper::Response;
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_RANGE, IF_RANGE, RANGE};
use peregrine_api::{
    ApiError, DownloadJob, DownloadOutcome, DownloadProgress, IfRangeValidator, SharedProgressSink,
};
use std::path::Path;
use std::time::Duration;
use url::Url;

/// How long a download may stall WITHOUT producing a frame before we
/// give up (not a total cap — downloads run as long as they make
/// progress). Unlike PROBE_TIMEOUT this is per-read, reset every frame.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

impl HttpEngine {
    /// Content-Range "bytes S-E/T" → (S, E, Some(T)); "bytes S-E/*" →
    /// (S, E, None). Non-conforming header → None. The END value feeds
    /// the segmenter's bounded-range check (B20): a 206 must match the
    /// range we asked for at BOTH ends before any byte is trusted.
    pub(crate) fn parse_content_range(
        headers: &hyper::HeaderMap,
    ) -> Option<(u64, u64, Option<u64>)> {
        let v = headers
            .get(CONTENT_RANGE)?
            .to_str()
            .ok()?
            .trim()
            .strip_prefix("bytes ")?;
        let (range, total) = v.split_once('/')?;
        let (start, end) = range.split_once('-')?;
        let start = start.parse::<u64>().ok()?;
        let end = end.parse::<u64>().ok()?;
        if end < start {
            return None;
        }
        let total = match total {
            "*" => None,
            t => Some(t.parse::<u64>().ok()?),
        };
        Some((start, end, total))
    }

    /// 416 bodies carry `Content-Range: bytes */T` (nginx, S3, …) — the
    /// server's authoritative total. Used to settle "is the resume
    /// offset already the whole file?" without trusting the probe.
    fn unsatisfiable_total(headers: &hyper::HeaderMap) -> Option<u64> {
        let v = headers
            .get(CONTENT_RANGE)?
            .to_str()
            .ok()?
            .trim()
            .strip_prefix("bytes */")?;
        v.parse::<u64>().ok()
    }

    /// The resource's CURRENT validator, as the final response states
    /// it. Feeds `DownloadOutcome::final_validator` so callers persist
    /// the fresh value (a 200-replay invalidates the old one).
    fn response_validator(headers: &hyper::HeaderMap) -> Option<IfRangeValidator> {
        if let Some(etag) = headers
            .get(hyper::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .filter(|t| !t.trim_start().starts_with("W/"))
        {
            return Some(IfRangeValidator::StrongEtag(etag));
        }
        headers
            .get(hyper::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|d| IfRangeValidator::LastModified(d.to_string()))
    }

    /// Open the sink for the chosen write mode. A resume into a
    /// missing file is a FRIENDLY error, not a bare ENOENT: the caller
    /// deleted the partial, or pointed at the wrong path (B19).
    async fn open_sink(path: &Path, mode: WriteMode) -> Result<tokio::fs::File, ApiError> {
        let file = match mode {
            WriteMode::Append => tokio::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .await
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => ApiError::Io(format!(
                        "resume target missing: {} — the partial file was deleted or the \
                         path is wrong; start a fresh download instead",
                        path.display()
                    )),
                    _ => ApiError::Io(format!("open {}: {e}", path.display())),
                })?,
            WriteMode::Truncate => tokio::fs::File::create(path).await?,
        };
        Ok(file)
    }
}

/// How the sink treats pre-existing bytes (B19: no stringly-typed mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    /// Confirmed resume: keep the prefix, append the remainder.
    Append,
    /// Fresh download or full replay: rewrite from zero.
    Truncate,
}

/// Issue a GET with optional `Range`/`If-Range`, chase redirects, and
/// return the FIRST non-redirect response (any status — success or
/// error; interpretation belongs to the caller) plus the final URL.
/// Shared by the single-stream downloader and every segment worker.
pub(crate) async fn fetch_get(
    client: &HttpsClient,
    max_redirects: usize,
    start: &Url,
    range: Option<&str>,
    if_range: Option<&str>,
) -> Result<(Response<Incoming>, Url), ApiError> {
    let mut current = start.clone();
    for hop in 0..=max_redirects {
        let mut builder = Request::builder()
            .method(hyper::Method::GET)
            .uri(current.as_str())
            .header(hyper::header::USER_AGENT, crate::USER_AGENT);
        if let Some(range) = range {
            builder = builder.header(RANGE, range);
        }
        if let Some(v) = if_range {
            builder = builder.header(IF_RANGE, v);
        }
        let req = builder
            .body(Full::new(Bytes::new()))
            .map_err(|e| ApiError::Network(format!("build request: {e}")))?;

        let res = client
            .request(req)
            .await
            .map_err(|e| ApiError::Network(format!("request {current}: {e}")))?;

        let status = res.status();
        if status.is_redirection() {
            if hop == max_redirects {
                return Err(ApiError::TooManyRedirects(current.to_string()));
            }
            let location = res
                .headers()
                .get(hyper::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    ApiError::Network(format!("{status} without Location: {current}"))
                })?;
            let next = current
                .join(location)
                .map_err(|e| ApiError::Network(format!("bad Location {location:?}: {e}")))?;
            if next == current {
                return Err(ApiError::Network(format!(
                    "redirect loop at {current} (Location points at itself)"
                )));
            }
            tracing::debug!(hop, from = %current, to = %next, "download redirect");
            current = next;
            continue;
        }
        return Ok((res, current));
    }
    unreachable!("loop returns on hop budget exhaustion")
}

pub(crate) async fn run_download(
    client: HttpsClient,
    max_redirects: usize,
    job: DownloadJob,
    progress: SharedProgressSink,
    cancel: tokio_util::sync::CancellationToken,
    budget: &peregrine_api::budget::BudgetChain,
    vstore: Option<&peregrine_storage::Store>,
) -> Result<DownloadOutcome, ApiError> {
    run_download_impl(
        client,
        max_redirects,
        job,
        progress,
        cancel,
        budget,
        false,
        vstore,
    )
    .await
}

// `healed`: set when this session is already the one-bounded restart
// issued by the 416 self-heal below — a second 416 then surfaces as an
// error instead of recursing (pathological server).
#[allow(clippy::too_many_arguments)]
async fn run_download_impl(
    client: HttpsClient,
    max_redirects: usize,
    job: DownloadJob,
    progress: SharedProgressSink,
    cancel: tokio_util::sync::CancellationToken,
    budget: &peregrine_api::budget::BudgetChain,
    healed: bool,
    vstore: Option<&peregrine_storage::Store>,
) -> Result<DownloadOutcome, ApiError> {
    // Fast-out on a pre-cancelled token (M2-b R2 P1-3): without this,
    // a paused task still pays the redirect chase and — worse — a
    // fresh (non-resume) call would truncate the sink via
    // WriteMode::Truncate before the body loop notices the token.
    if cancel.is_cancelled() {
        return Err(ApiError::Cancelled);
    }
    let DownloadJob {
        url,
        sink,
        resume,
        mut expected_total,
    } = job;
    // Same session-base declaration as the segmented path (see
    // there): single-stream resumes from `start_offset`.
    progress.on_session_base(resume.as_ref().map(|c| c.start_offset).unwrap_or(0));

    let current =
        Url::parse(&url).map_err(|e| ApiError::Network(format!("invalid url {url:?}: {e}")))?;

    // Redirect chase + terminal response (shared with segment workers).
    let range = resume
        .as_ref()
        .map(|c| format!("bytes={}-", c.start_offset));
    let if_range = resume
        .as_ref()
        .and_then(|c| c.validator.as_ref())
        .map(|v| match v {
            IfRangeValidator::StrongEtag(e) => e.as_str(),
            IfRangeValidator::LastModified(d) => d.as_str(),
        });
    let (res, final_url) =
        fetch_get(&client, max_redirects, &current, range.as_deref(), if_range).await?;
    let current = final_url;

    let status = res.status();
    if !status.is_success() {
        // 416 settling "already complete": prefer the server's own
        // `Content-Range: bytes */T` (authoritative), fall back to
        // the probe's expected_total. Without either we cannot call
        // it complete — surface the 416 as an error instead of
        // guessing (a wrong "complete" is the worst failure mode).
        if status == hyper::StatusCode::RANGE_NOT_SATISFIABLE {
            let server_total = HttpEngine::unsatisfiable_total(res.headers());
            let settled = server_total
                .or(expected_total)
                .filter(|total| resume.as_ref().is_some_and(|c| c.start_offset == *total));
            if let Some(total) = settled {
                progress.on_progress(&peregrine_api::DownloadProgress {
                    bytes_done: total,
                    total: Some(total),
                });
                return Ok(DownloadOutcome {
                    bytes_written: 0,
                    total_bytes: Some(total),
                    completed: true,
                    final_url: current.to_string(),
                    final_validator: HttpEngine::response_validator(res.headers()),
                });
            }
            // Self-heal (QA-E2E Bug 3): a 416 whose offset does NOT
            // settle as "already complete" means the resume context
            // is a lie about the sink — the canonical case is a
            // sparse-preallocated file (len == total) whose segment
            // rows were purged on remove, feeding `start_offset ==
            // total` into a fresh single-stream re-add; mirrors that
            // serve a different resource or a truncated sink land
            // here too. One bounded restart from zero beats a task
            // that can only ever 416. The retry re-enters with
            // resume = None, so the fresh 200 path truncates the
            // stale sink and rewrites it; if a server ever answers
            // the retry's 416-less request with 416 anyway, `healed`
            // stops the recursion.
            if !healed && resume.as_ref().is_some_and(|c| c.start_offset > 0) {
                let stale = resume.as_ref().map(|c| c.start_offset).unwrap_or(0);
                tracing::warn!(
                    offset = stale,
                    total = server_total.or(expected_total),
                    url = %current,
                    "416 with non-settling offset — stale resume context (sparse/mutated sink), \
                     restarting from zero"
                );
                drop(res); // release the pooled connection before the retry
                let healed_job = DownloadJob {
                    url: url.clone(),
                    sink: sink.clone(),
                    resume: None,
                    expected_total,
                };
                return Box::pin(run_download_impl(
                    client.clone(),
                    max_redirects,
                    healed_job,
                    progress,
                    cancel,
                    budget,
                    true,
                    vstore,
                ))
                .await;
            }
        }
        return Err(ApiError::Http {
            status: status.as_u16(),
            url: current.to_string(),
        });
    }

    let headers = res.headers().clone();
    let response = res;

    let (write_mode, resume_offset, server_total) = match (resume.as_ref(), status.as_u16()) {
        // Confirmed partial: server honored our offset.
        (Some(ctx), 206) => {
            let (start, end, total) =
                HttpEngine::parse_content_range(&headers).ok_or_else(|| {
                    ApiError::Network(format!("206 without parseable Content-Range: {current}"))
                })?;
            if start != ctx.start_offset {
                return Err(ApiError::Network(format!(
                    "server answered 206 from byte {start}, expected {} — refusing to glue \
                     mismatched bytes (possible concurrent modification)",
                    ctx.start_offset
                )));
            }
            // Second guard (B20): for an open range the announced end
            // must be total-1; anything else means the server capped
            // or mangled the range — short-read detection still holds,
            // but log it so it is visible in telemetry.
            if let Some(total) = total.filter(|t| end + 1 != *t) {
                tracing::warn!(end, total, "206 end does not match total-1 for open range");
            }
            // A server that IGNORES If-Range (some only honor Range)
            // answers 206 even for a changed resource. If the 206 carries
            // an ETag that differs from the validator we sent, the bytes
            // are from a different version — refuse rather than glue.
            if let (Some(sent), Some(served)) = (
                ctx.validator.as_ref(),
                headers
                    .get(hyper::header::ETAG)
                    .and_then(|v| v.to_str().ok()),
            ) {
                let sent_val = match sent {
                    IfRangeValidator::StrongEtag(e) => e.as_str(),
                    IfRangeValidator::LastModified(d) => d.as_str(),
                };
                if sent_val != served {
                    return Err(ApiError::Network(format!(
                        "resource changed mid-resume: If-Range validator {sent_val:?} no \
                         longer matches served etag {served:?} — refusing to glue"
                    )));
                }
            }
            (WriteMode::Append, ctx.start_offset, total)
        }
        // Full replay: server ignored Range or If-Range rejected the
        // resume (resource changed) — restart from zero, overwrite.
        (Some(_), 200) => (WriteMode::Truncate, 0, headers_content_length(&headers)),
        // No resume requested: a fresh 200. An unexpected 206 (we sent
        // no Range) is a server bug — refuse instead of trusting a range
        // we never asked for.
        (None, 200) => (WriteMode::Truncate, 0, headers_content_length(&headers)),
        (None, 206) => {
            return Err(ApiError::Network(format!(
                "206 for a request without Range — server bug: {current}"
            )));
        }
        (Some(_), other) => {
            return Err(ApiError::Network(format!(
                "unexpected status {other} for ranged download: {current}"
            )));
        }
        // Success codes other than 200/206 (204 No Content, …) carry no
        // body semantics we can honor — refuse rather than write nothing
        // and call it complete.
        (_, other) => {
            return Err(ApiError::Network(format!(
                "unexpected status {other} for download: {current}"
            )));
        }
    };

    if let Some(t) = server_total {
        // Content-Range/Length is authoritative over the probe's number.
        if expected_total != Some(t) {
            tracing::debug!(probe = ?expected_total, actual = t, "total size corrected");
        }
        expected_total = Some(t);
    }

    // B36 write side: persist THIS response's validator (strong
    // etag / Last-Modified, wire form) keyed by (url, sink) at the
    // FIRST response of the session — so even a pause/crash right
    // after these headers leaves the row carrying the validator the
    // NEXT session should send as `If-Range`. A changed remote then
    // answers 200 (If-Range rejected) → the `Truncate` branch above
    // rewrites from zero instead of gluing a mixed body. Best
    // effort: a store failure degrades to validator-less resume
    // (today's behavior), never fails the download.
    //
    // `upsert_validator_only` creates a total=NULL row (Route 1
    // keys segmented resume on `total IS NOT NULL`, so a
    // single-stream row must never carry one) and, on conflict,
    // refreshes ONLY the etag — a concurrent segmented session's
    // total-bearing row is never clobbered (R2 P1-1).
    if let Some(store) = vstore
        && let Some(v) = HttpEngine::response_validator(&headers)
    {
        let wire = v.wire().to_string();
        if let Err(e) = store.upsert_validator_only(&url, &sink, &wire).await {
            tracing::warn!(error = %e, url, "persisting resume validator failed");
        }
    }

    let mut file = HttpEngine::open_sink(&sink, write_mode).await?;
    // The one silent-corruption hole left: append assumes the disk file
    // is EXACTLY `start_offset` long. A shorter/longer file would glue
    // the 206 body at the wrong position. Verified here — engine
    // guarantee, not caller discipline.
    if write_mode == WriteMode::Append {
        let on_disk = file
            .metadata()
            .await
            .map_err(|e| ApiError::Io(format!("stat {}: {e}", sink.display())))?
            .len();
        let wanted = resume
            .as_ref()
            .map(|c| c.start_offset)
            .expect("append mode implies a resume context");
        if on_disk != wanted {
            return Err(ApiError::Io(format!(
                "resume mismatch: sink {} has {on_disk} bytes, resume context says {wanted}",
                sink.display()
            )));
        }
    }
    let mut body = response.into_body();

    let mut written: u64 = 0;
    loop {
        let frame = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Cooperative stop (user pause / shutdown): flush what
                // landed and leave a clean partial. The next resume
                // Range-continues from the verified on-disk length.
                // A final progress event lets the scheduler's
                // received_bytes converge without re-stat (P2-6).
                use tokio::io::AsyncWriteExt;
                file.flush()
                    .await
                    .map_err(|e| ApiError::Io(format!("flush {}: {e}", sink.display())))?;
                progress.on_progress(&DownloadProgress {
                    bytes_done: resume_offset + written,
                    total: expected_total,
                });
                return Err(ApiError::Cancelled);
            }
            frame = tokio::time::timeout(STALL_TIMEOUT, body.frame()) => frame
                .map_err(|_| {
                    ApiError::Network(format!(
                        "download stalled: no data for {}s (got {} of {:?})",
                        STALL_TIMEOUT.as_secs(),
                        resume_offset + written,
                        expected_total
                    ))
                })?
                .transpose()
                .map_err(|e| ApiError::Network(format!("body read {current}: {e}")))?,
        };

        let Some(frame) = frame else { break }; // clean EOF

        if let Some(chunk) = frame.data_ref().filter(|c| !c.is_empty()) {
            // Slice a frame LARGER than one second's budget into
            // cap-sized writes: a single acquire for a 1.5 MiB hyper
            // frame at 128 KiB/s parks ~12 s with zero bytes written
            // (progress goes dark — the M3-b1 smoke caught this).
            // Slice-wise payment keeps progress ~1 Hz and the write
            // stream smooth. Unlimited → hint is u64::MAX → one slice.
            let hint = budget.slice_hint();
            let mut rest = chunk.as_ref();
            while !rest.is_empty() {
                let take = (rest.len() as u64).min(hint) as usize;
                // Rate budget (M3-b): park for affordability BEFORE the
                // write. The park itself is cancellable — a pause during
                // a long throttle park (tiny bps, big chunk) must flush
                // the partial, not hang the worker until the budget
                // fills. Budget debits only after a completed park, so a
                // cancelled park leaks nothing.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        use tokio::io::AsyncWriteExt;
                        file.flush()
                            .await
                            .map_err(|e| ApiError::Io(format!("flush {}: {e}", sink.display())))?;
                        progress.on_progress(&DownloadProgress {
                            bytes_done: resume_offset + written,
                            total: expected_total,
                        });
                        return Err(ApiError::Cancelled);
                    }
                    _ = budget.acquire(take as u64) => {}
                }
                use tokio::io::AsyncWriteExt;
                file.write_all(&rest[..take])
                    .await
                    .map_err(|e| ApiError::Io(format!("write {}: {e}", sink.display())))?;
                written += take as u64;
                progress.on_progress(&peregrine_api::DownloadProgress {
                    bytes_done: resume_offset + written,
                    total: expected_total,
                });
                rest = &rest[take..];
            }
        }
        // trailers (and any future frame kinds) are skipped.
    }

    use tokio::io::AsyncWriteExt;
    file.flush()
        .await
        .map_err(|e| ApiError::Io(format!("flush {}: {e}", sink.display())))?;
    // Durability against power loss; kill -9 is already covered by the
    // page cache. One fsync per download session, not per chunk.
    file.sync_all()
        .await
        .map_err(|e| ApiError::Io(format!("sync {}: {e}", sink.display())))?;

    let bytes_done = resume_offset + written;
    // Final, always-emitted progress: a 0-byte body emits NO per-chunk
    // events, and unknown-size streams never see a total until now.
    // Callers can key "100%" off the completed outcome instead.
    progress.on_progress(&peregrine_api::DownloadProgress {
        bytes_done,
        total: expected_total,
    });
    let completed = match expected_total {
        Some(total) if bytes_done == total => true,
        Some(total) => {
            return Err(ApiError::Network(format!(
                "short read: {} of {} bytes from {current}",
                bytes_done, total
            )));
        }
        None => true, // unknown size: EOF is all we can ask for
    };

    Ok(DownloadOutcome {
        bytes_written: written,
        total_bytes: expected_total,
        completed,
        final_url: current.to_string(),
        final_validator: HttpEngine::response_validator(&headers),
    })
}

fn headers_content_length(headers: &hyper::HeaderMap) -> Option<u64> {
    headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}
