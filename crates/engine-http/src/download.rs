//! Single-connection streamed download with resume semantics (M1-b).
//!
//! The contract: bytes land on disk (append for a confirmed resume,
//! truncate-and-rewrite when the server replays the full body), progress
//! is cumulative (resume offset included), and every exit path leaves
//! either a complete file or a VALID PARTIAL file the caller can resume
//! from. Cancellation is future-drop: writes already issued to the OS
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
    ApiError, DownloadJob, DownloadOutcome, IfRangeValidator, ResumeContext, SharedProgressSink,
};
use std::path::Path;
use std::time::Duration;
use url::Url;

/// How long a download may stall WITHOUT producing a frame before we
/// give up (not a total cap — downloads run as long as they make
/// progress). Unlike PROBE_TIMEOUT this is per-read, reset every frame.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);

impl HttpEngine {
    /// Content-Range "bytes S-E/T" → (S, Some(T)); "bytes S-E/*" → (S, None).
    /// Non-conforming header → None.
    pub(crate) fn parse_content_range(headers: &hyper::HeaderMap) -> Option<(u64, Option<u64>)> {
        let v = headers
            .get(CONTENT_RANGE)?
            .to_str()
            .ok()?
            .trim()
            .strip_prefix("bytes ")?;
        let (range, total) = v.split_once('/')?;
        let start = range.split_once('-')?.0.parse::<u64>().ok()?;
        let total = match total {
            "*" => None,
            t => Some(t.parse::<u64>().ok()?),
        };
        Some((start, total))
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

    /// Open the sink for the chosen write mode.
    async fn open_sink(path: &Path, append: bool) -> Result<tokio::fs::File, ApiError> {
        let file = if append {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .await?
        } else {
            tokio::fs::File::create(path).await?
        };
        Ok(file)
    }
}

pub(crate) async fn run_download(
    client: HttpsClient,
    max_redirects: usize,
    job: DownloadJob,
    progress: SharedProgressSink,
) -> Result<DownloadOutcome, ApiError> {
    let DownloadJob {
        url,
        sink,
        resume,
        mut expected_total,
    } = job;

    let mut current =
        Url::parse(&url).map_err(|e| ApiError::Network(format!("invalid url {url:?}: {e}")))?;

    // --- chase redirects (GET; headers-only until the final hop) -------
    // Budget mirrors the engine's probe policy: bounded hops, loop-safe
    // (a plain A→B→A alternation defeats equality checks, so count hops).
    let mut response: Option<Response<Incoming>> = None;
    'chase: for hop in 0..=max_redirects {
        let mut builder = Request::builder()
            .method(hyper::Method::GET)
            .uri(current.as_str());

        // Resume → Range + If-Range (validator strength enforced at
        // ResumeContext construction; here we just forward it).
        if let Some(ResumeContext {
            start_offset,
            validator,
        }) = resume.as_ref()
        {
            builder = builder.header(RANGE, format!("bytes={start_offset}-"));
            if let Some(v) = validator {
                let v = match v {
                    IfRangeValidator::StrongEtag(e) => e.as_str(),
                    IfRangeValidator::LastModified(d) => d.as_str(),
                };
                builder = builder.header(IF_RANGE, v);
            }
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
            }
            return Err(ApiError::Http {
                status: status.as_u16(),
                url: current.to_string(),
            });
        }

        response = Some(res);
        break 'chase;
    }

    let response = response.expect("chase loop returns or breaks with a response");

    let status = response.status();
    let headers = response.headers().clone();

    let (write_mode, resume_offset, server_total) = match (resume.as_ref(), status.as_u16()) {
        // Confirmed partial: server honored our offset.
        (Some(ctx), 206) => {
            let (start, total) = HttpEngine::parse_content_range(&headers).ok_or_else(|| {
                ApiError::Network(format!("206 without parseable Content-Range: {current}"))
            })?;
            if start != ctx.start_offset {
                return Err(ApiError::Network(format!(
                    "server answered 206 from byte {start}, expected {} — refusing to glue \
                     mismatched bytes (possible concurrent modification)",
                    ctx.start_offset
                )));
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
            ("append", ctx.start_offset, total)
        }
        // Full replay: server ignored Range or If-Range rejected the
        // resume (resource changed) — restart from zero, overwrite.
        (Some(_), 200) => ("truncate", 0, headers_content_length(&headers)),
        // No resume requested: a fresh 200. An unexpected 206 (we sent
        // no Range) is a server bug — refuse instead of trusting a range
        // we never asked for.
        (None, 200) => ("truncate", 0, headers_content_length(&headers)),
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

    let append = write_mode == "append";
    let mut file = HttpEngine::open_sink(&sink, append).await?;
    // The one silent-corruption hole left: append assumes the disk file
    // is EXACTLY `start_offset` long. A shorter/longer file would glue
    // the 206 body at the wrong position. Verified here — engine
    // guarantee, not caller discipline.
    if append {
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
        let frame = tokio::time::timeout(STALL_TIMEOUT, body.frame())
            .await
            .map_err(|_| {
                ApiError::Network(format!(
                    "download stalled: no data for {}s (got {} of {:?})",
                    STALL_TIMEOUT.as_secs(),
                    resume_offset + written,
                    expected_total
                ))
            })?
            .transpose()
            .map_err(|e| ApiError::Network(format!("body read {current}: {e}")))?;

        let Some(frame) = frame else { break }; // clean EOF

        if let Some(chunk) = frame.data_ref().filter(|c| !c.is_empty()) {
            use tokio::io::AsyncWriteExt;
            file.write_all(chunk)
                .await
                .map_err(|e| ApiError::Io(format!("write {}: {e}", sink.display())))?;
            written += chunk.len() as u64;
            progress.on_progress(&peregrine_api::DownloadProgress {
                bytes_done: resume_offset + written,
                total: expected_total,
            });
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
