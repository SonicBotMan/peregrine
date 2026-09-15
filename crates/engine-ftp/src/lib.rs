//! engine-ftp — passive-mode FTP downloader (M4-c, PROPOSAL W10).
//!
//! One control connection per download; data transfers always go
//! through PASV (v1 refuses FTPS — see BACKLOG). The pipeline:
//!
//! ```text
//! parse URL → connect (15s) → login → TYPE I → SIZE
//!           → resume? REST offset
//!           → RETR (PASV data stream) → framed copy → 226 → verify
//! ```
//!
//! Resume semantics: FTP has no etag/If-Range. The authority is
//! `SIZE`: if the file is now SHORTER than our resume offset the
//! remote changed — restart from zero (truncate), exactly like an
//! HTTP 200-replay. A longer or equal file resumes from the offset.
//! The sink-length verification before append is the same engine
//! guarantee the HTTP engine makes: append only ever glues onto a
//! prefix of exactly `start_offset` bytes.
//!
//! Short reads (data connection closes before SIZE's total) are
//! ERRORS, not completions — same rule as every other engine: a
//! "finished" file that is silently truncated is the worst outcome.

use peregrine_api::download::{DownloadJob, DownloadOutcome, DownloadProgress, SharedProgressSink};
use peregrine_api::engine::{ProbeFuture, ProtocolEngine};
use peregrine_api::{ApiError, ProbeInfo};
use std::path::Path;
use std::time::Duration;
use suppaftp::FtpError;
use suppaftp::tokio::AsyncFtpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Control/data connection establishment budget. Commands after
/// connect are one RTT each on an open connection — no extra cap.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-frame stall budget, reset every chunk (mirrors engine-http).
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-probe budget (M4-b1.1 R2' parity: a wedged origin must not
/// park a daemon probe slot forever).
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Read-frame buffer — FTP data has no framing, this is just I/O
/// granularity. 64 KiB keeps progress ~Hz at throttled rates.
const FRAME_BUF: usize = 64 * 1024;

pub struct FtpEngine {
    /// Anonymous fallback identity when the URL carries no userinfo.
    anonymous_user: String,
    anonymous_pass: String,
}

impl Default for FtpEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FtpEngine {
    pub fn new() -> Self {
        Self {
            // RFC 1638 §4: anonymous "should" give a password the
            // server can log (an e-mail-ish token). A neutral app
            // identity beats a fake address.
            anonymous_user: "anonymous".into(),
            anonymous_pass: "peregrine@localhost".into(),
        }
    }

    /// Connect + login + binary mode. Shared by probe and download.
    async fn session(target: &FtpTarget, anon: (&str, &str)) -> Result<AsyncFtpStream, ApiError> {
        let addr = (target.host.as_str(), target.port);
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(addr)
            .await
            .map_err(|e| {
                ApiError::Network(format!("resolve {}:{}: {e}", target.host, target.port))
            })?
            .collect();
        let addr = addrs.first().ok_or_else(|| {
            ApiError::Network(format!("no addresses for {}:{}", target.host, target.port))
        })?;
        let mut ftp = AsyncFtpStream::connect_timeout(*addr, CONNECT_TIMEOUT)
            .await
            .map_err(|e| ftp_err("connect", e))?;
        // Anonymous fallback only when NO user is given. A named
        // user with no password gets "" (curl semantics) — sending
        // the anon identity as a personal password is a 530 on
        // bookmark-style URLs (R2 P2-3).
        let user = target
            .username
            .clone()
            .unwrap_or_else(|| anon.0.to_string());
        let pass = match (&target.username, &target.password) {
            (_, Some(p)) => p.clone(),
            (Some(_), None) => String::new(),
            (None, None) => anon.1.to_string(),
        };
        ftp.login(user, pass)
            .await
            .map_err(|e| ftp_err("login", e))?;
        ftp.transfer_type(suppaftp::types::FileType::Binary)
            .await
            .map_err(|e| ftp_err("TYPE I", e))?;
        Ok(ftp)
    }
}

fn ftp_err(stage: &str, e: FtpError) -> ApiError {
    ApiError::Network(format!("ftp {stage}: {e}"))
}

/// A parsed `ftp://` target. Percent-encoding is decoded for the
/// userinfo and path (URL-encoded credentials/paths are the norm in
/// bookmarks and shell histories).
struct FtpTarget {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    /// Remote path, guaranteed to start with `/` (URL paths always
    /// do; the defense below only covers a degenerate empty path —
    /// `//` is NOT collapsed, a leading-empty component is a legal,
    /// distinct remote path).
    path: String,
}

fn parse_url(url: &str) -> Result<FtpTarget, ApiError> {
    let u = Url::parse(url).map_err(|e| ApiError::Network(format!("invalid url {url:?}: {e}")))?;
    if u.scheme() != "ftp" {
        return Err(ApiError::UnsupportedUrl(format!(
            "ftp engine got {url:?} (scheme {:?})",
            u.scheme()
        )));
    }
    let host = u
        .host_str()
        .ok_or_else(|| ApiError::Network(format!("ftp url without host: {url}")))?
        .to_string();
    // userinfo components keep their percent-encoding in url::Url;
    // one unconditional decode pass restores them (lossy for
    // non-UTF-8 sequences — acceptable for v1, noted in BACKLOG).
    let decode = |s: &str| -> String { percent_decode(s) };
    let username = (!u.username().is_empty()).then(|| decode(u.username()));
    let password = u.password().map(decode);
    // `u.path()` keeps percent-encoding; decode once. FTP URLs carry
    // no query — a stray one would be part of the remote filename.
    let path = percent_decode(u.path());
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    Ok(FtpTarget {
        host,
        port: u.port().unwrap_or(21),
        username,
        password,
        path,
    })
}

/// One-shot percent-decoding (`%XX`), leaving everything else intact.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    while i < bytes.len() {
        // Need two hex digits after '%': indices i+1 and i+2 must
        // both be in bounds (i + 2 < len implies both).
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Filename for the sink suggestion: last path component, if any.
fn remote_filename(path: &str) -> Option<String> {
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

impl ProtocolEngine for FtpEngine {
    fn name(&self) -> &'static str {
        "ftp"
    }

    fn supports(&self, url: &str) -> bool {
        Url::parse(url)
            .map(|u| u.scheme() == "ftp")
            .unwrap_or(false)
    }

    fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>> {
        let url = url.to_string();
        // Same identity policy as download (R2 P2-2): the engine
        // fields, not a drifting second copy.
        let anon_user = self.anonymous_user.clone();
        let anon_pass = self.anonymous_pass.clone();
        Box::pin(async move {
            let target = parse_url(&url)?;
            let size = tokio::time::timeout(PROBE_TIMEOUT, async {
                let mut ftp = FtpEngine::session(&target, (&anon_user, &anon_pass)).await?;
                // SIZE can legitimately fail (server without SIZE
                // support, or ASCII-mode-only implementations):
                // probe still succeeds, just without a length.
                let size = ftp.size(&target.path).await.ok().map(|n| n as u64);
                let _ = ftp.quit().await;
                Ok::<_, ApiError>(size)
            })
            .await
            .map_err(|_| ApiError::Network("ftp probe: timed out after 30s".into()))??;
            Ok(ProbeInfo {
                url,
                // None is fine — download then runs unknown-total.
                content_length: size,
                // REST is honored by every server that answers SIZE,
                // and we only resume after a successful SIZE — but we
                // do not parallelize FTP v1 anyway; single stream.
                accept_ranges: size.is_some(),
                etag: None,
                etag_strong: false,
                last_modified: None,
                filename: remote_filename(&target.path),
            })
        })
    }

    fn download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
        budget: &peregrine_api::budget::BudgetChain,
    ) -> peregrine_api::DownloadFuture<Result<DownloadOutcome, ApiError>> {
        // BudgetChain and the anon credentials are cloned in: the
        // boxed future needs 'static (two Arcs + two small Strings).
        let budget = budget.clone();
        let anon_user = self.anonymous_user.clone();
        let anon_pass = self.anonymous_pass.clone();
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(ApiError::Cancelled);
            }
            let DownloadJob {
                url,
                sink,
                resume,
                expected_total,
            } = job;

            let target = parse_url(&url)?;
            let mut ftp = FtpEngine::session(&target, (&anon_user, &anon_pass)).await?;

            // SIZE is the resume authority (no etags in FTP).
            let remote_size = ftp.size(&target.path).await.ok().map(|n| n as u64);

            // Resume decision: remote shorter than our offset means
            // the file changed — restart from zero (HTTP 200-replay
            // semantics). Without SIZE we cannot validate anything:
            // an unvalidated append would glue onto a stale prefix —
            // refuse the resume, restart clean.
            let (resume_offset, truncate) = match (&resume, remote_size) {
                (Some(ctx), Some(size)) if size >= ctx.start_offset && ctx.start_offset > 0 => {
                    (ctx.start_offset, false)
                }
                (Some(_), _) => (0, true),
                (None, _) => (0, true),
            };

            progress.on_session_base(resume_offset);

            if resume_offset > 0 {
                // usize cast: FTP offsets are 32-bit-ish in practice;
                // a >usize::MAX file cannot exist on a 64-bit-target
                // build anyway (u64 == usize there).
                let off = usize::try_from(resume_offset).map_err(|_| {
                    ApiError::Network(format!("resume offset {resume_offset} too large"))
                })?;
                ftp.resume_transfer(off)
                    .await
                    .map_err(|e| ftp_err("REST", e))?;
            }

            // P1-2 (R2): PASV→RETR→data-connect is a bare connect in
            // suppaftp (no timeout, no cancel). A NAT-mangled PASV
            // address parks here for SYN-retry minutes. Budgeted and
            // cancellable like everything else; on cancel the session
            // is simply dropped (no QUIT — see the cancel arm below).
            let mut stream = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ApiError::Cancelled),
                r = tokio::time::timeout(CONNECT_TIMEOUT, ftp.retr_as_stream(&target.path)) => r
                    .map_err(|_| ApiError::Network("opening data connection timed out".into()))?
                    .map_err(|e| ftp_err("RETR", e))?,
            };

            let mut file = open_sink(&sink, truncate).await?;
            if !truncate {
                // Engine guarantee (same as HTTP): append only onto a
                // prefix of EXACTLY resume_offset bytes.
                let on_disk = file
                    .metadata()
                    .await
                    .map_err(|e| ApiError::Io(format!("stat {}: {e}", sink.display())))?
                    .len();
                if on_disk != resume_offset {
                    return Err(ApiError::Io(format!(
                        "resume mismatch: sink {} has {on_disk} bytes, resume context says {resume_offset}",
                        sink.display()
                    )));
                }
            }

            let total = remote_size.or(expected_total);
            let mut written: u64 = 0;
            let mut buf = vec![0u8; FRAME_BUF];
            loop {
                let n = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        file.flush()
                            .await
                            .map_err(|e| ApiError::Io(format!("flush {}: {e}", sink.display())))?;
                        progress.on_progress(&DownloadProgress {
                            bytes_done: resume_offset + written,
                            total,
                        });
                        // Drop mid-transfer: the sink is flushed and
                        // progress reported above. No QUIT — a QUIT on
                        // a dangling transfer reads a reply the server
                        // may never send (R2 P1-1); dropping the
                        // session is instant and the server times it
                        // out on its own.
                        drop(stream);
                        drop(ftp);
                        return Err(ApiError::Cancelled);
                    }
                    r = tokio::time::timeout(STALL_TIMEOUT, stream.read(&mut buf)) => r
                        .map_err(|_| {
                            ApiError::Network(format!(
                                "download stalled: no data for {}s (got {} of {total:?})",
                                STALL_TIMEOUT.as_secs(),
                                resume_offset + written,
                            ))
                        })?
                        .map_err(|e| ApiError::Network(format!("data read: {e}")))?,
                };
                if n == 0 {
                    break; // data connection closed — normal EOF
                }
                // Same slice-wise budget payment as engine-http: keep
                // progress ~1 Hz under throttle, cancellable park.
                let hint = budget.slice_hint();
                let mut rest = &buf[..n];
                while !rest.is_empty() {
                    let take = (rest.len() as u64).min(hint) as usize;
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            file.flush().await.map_err(|e| {
                                ApiError::Io(format!("flush {}: {e}", sink.display()))
                            })?;
                            progress.on_progress(&DownloadProgress {
                                bytes_done: resume_offset + written,
                                total,
                            });
                            drop(stream);
                            drop(ftp);
                            return Err(ApiError::Cancelled);
                        }
                        _ = budget.acquire(take as u64) => {}
                    }
                    file.write_all(&rest[..take])
                        .await
                        .map_err(|e| ApiError::Io(format!("write {}: {e}", sink.display())))?;
                    written += take as u64;
                    progress.on_progress(&DownloadProgress {
                        bytes_done: resume_offset + written,
                        total,
                    });
                    rest = &rest[take..];
                }
            }

            // EOF on the data connection: the server must now send
            // 226 (transfer complete). finish() reads it; anything
            // else (426 …) is a failed transfer — surface it.
            // A bare control read can hang forever on a dead server
            // (R2 P1-1): budget it. The data is already synced, so a
            // timeout here is still a completed body — treat the
            // missing confirmation as a Network error the scheduler
            // retry can reconcile against SIZE on the next attempt.
            tokio::time::timeout(Duration::from_secs(10), stream.finish())
                .await
                .map_err(|_| ApiError::Network("awaiting 226: timed out".into()))?
                .map_err(|e| ftp_err("await 226", e))?;

            file.flush()
                .await
                .map_err(|e| ApiError::Io(format!("flush {}: {e}", sink.display())))?;
            file.sync_all()
                .await
                .map_err(|e| ApiError::Io(format!("sync {}: {e}", sink.display())))?;
            // Post-sync QUIT is pure politeness (R2 P1-1): budget it
            // and ignore the outcome — everything is already on disk.
            let _ = tokio::time::timeout(Duration::from_secs(10), ftp.quit()).await;

            let bytes_done = resume_offset + written;
            if let Some(size) = total.filter(|t| bytes_done != *t) {
                // B56: SIZE-vs-RETR skew is normal (remote appended
                // or rotated between the two commands) — say WHICH
                // way it skewed instead of a bare "short body".
                let skew = if bytes_done < size {
                    format!(
                        "short body: got {bytes_done} of {size} bytes (remote shrank or \
                         connection dropped early)"
                    )
                } else {
                    format!(
                        "body longer than SIZE: got {bytes_done} of {size} bytes \
                         (remote grew between SIZE and RETR)"
                    )
                };
                return Err(ApiError::Network(skew));
            }
            progress.on_progress(&DownloadProgress { bytes_done, total });
            Ok(DownloadOutcome {
                bytes_written: written,
                total_bytes: total.or(Some(bytes_done)),
                completed: true,
                final_url: url,
                final_validator: None,
                replayed_from_zero: false,
            })
        })
    }
}

/// Truncate-or-append sink opening (mirrors engine-http's WriteMode).
async fn open_sink(sink: &Path, truncate: bool) -> Result<tokio::fs::File, ApiError> {
    let file = if truncate {
        tokio::fs::File::create(sink)
            .await
            .map_err(|e| ApiError::Io(format!("create {}: {e}", sink.display())))?
    } else {
        // Append mode positions writes at end-of-file; combined with
        // the length verification above, bytes glue exactly.
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(sink)
            .await
            // B52: align with engine-http B19 — a resume into a
            // missing file is a FRIENDLY error, not a bare ENOENT.
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ApiError::Io(format!(
                    "resume target missing: {} — the partial file was deleted or the \
                     path is wrong; start a fresh download instead",
                    sink.display()
                )),
                _ => ApiError::Io(format!("open {}: {e}", sink.display())),
            })?
    };
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_userinfo_port_and_defaults() {
        let t = parse_url("ftp://files.example.com/pub/big.iso").unwrap();
        assert_eq!(t.host, "files.example.com");
        assert_eq!(t.port, 21);
        assert_eq!(t.path, "/pub/big.iso");
        assert!(t.username.is_none() && t.password.is_none());

        let t = parse_url("ftp://user:p%40ss@h:2121/a%20b.zip").unwrap();
        assert_eq!(t.username.as_deref(), Some("user"));
        assert_eq!(t.password.as_deref(), Some("p@ss"));
        assert_eq!(t.port, 2121);
        assert_eq!(t.path, "/a b.zip");
    }

    #[test]
    fn rejects_non_ftp() {
        assert!(parse_url("http://x/y").is_err());
        assert!(parse_url("ftp://").is_err());
    }

    #[test]
    fn supports_scheme_only() {
        let e = FtpEngine::new();
        assert!(e.supports("ftp://h/f"));
        assert!(!e.supports("ftps://h/f"));
        assert!(!e.supports("http://h/f"));
        assert!(!e.supports("garbage"));
    }

    #[test]
    fn filename_extraction() {
        assert_eq!(
            remote_filename("/pub/x.tar.gz").as_deref(),
            Some("x.tar.gz")
        );
        assert_eq!(remote_filename("/").as_deref(), None);
    }
}
