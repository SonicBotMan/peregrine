//! HTTP/HTTPS protocol engine (architecture rule #2: protocol = trait).
//!
//! `probe` is the M1 pipeline's phase 1, in two rounds:
//!
//! 1. **HEAD** the URL, follow up to
//!    [`HttpEngine::DEFAULT_MAX_REDIRECTS`] redirects, and collect what the
//!    server *claims*: resumability (`Accept-Ranges`), size (`Content-Length`),
//!    integrity (`ETag`/`Last-Modified`) and filename.
//! 2. **Confirm with a real ranged GET** (`Range: bytes=0-0`, BACKLOG B7):
//!    servers that claim `Accept-Ranges: bytes` but ignore `Range` get
//!    downgraded to single-segment; servers that never advertise the header
//!    but honor `Range` get upgraded. A `206` answer also yields the
//!    authoritative total size from `Content-Range`.
//!
//! The engine owns one pooled client; probes reuse connections. TLS via
//! rustls (ring provider) — no system OpenSSL dependency.

pub(crate) mod download;
pub mod segment;

pub use segment::{SegmentConfig, plan_ranges};

use http_body_util::Full;
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED,
    LOCATION, RANGE,
};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use peregrine_api::engine::{ProbeFuture, ProbeInfo};
use peregrine_api::{ApiError, ProtocolEngine};
use std::time::Duration;
use url::Url;

/// Whole probe chain budget (HEAD redirect chase + range confirm, all
/// hops included — enforced with a single deadline, not per-request).
/// Downloads are long; probes must not be.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

/// An HTTP(S) engine with a shared, pooled client.
pub struct HttpEngine {
    client: HttpsClient,
    max_redirects: usize,
}

impl HttpEngine {
    /// Follow at most this many 3xx hops before giving up.
    pub const DEFAULT_MAX_REDIRECTS: usize = 5;

    /// Engine with default settings (5 redirect hops).
    pub fn new() -> Result<Self, ApiError> {
        Self::with_max_redirects(Self::DEFAULT_MAX_REDIRECTS)
    }

    /// Engine with a custom redirect budget (0 = don't follow).
    pub fn with_max_redirects(max_redirects: usize) -> Result<Self, ApiError> {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| ApiError::Internal(format!("load native TLS roots: {e}")))?
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(https);
        Ok(Self {
            client,
            max_redirects,
        })
    }
}

impl HttpEngine {
    /// Segmented download (PROPOSAL §5): static ranges, bounded worker
    /// pool, per-segment cursors persisted in `store`. Requires a known
    /// total — the caller (probe layer) must route unknown-size or
    /// non-ranged downloads to [`ProtocolEngine::download`].
    pub async fn download_segmented(
        &self,
        job: peregrine_api::DownloadJob,
        cfg: &segment::SegmentConfig,
        store: &peregrine_storage::Store,
        progress: peregrine_api::SharedProgressSink,
    ) -> Result<peregrine_api::DownloadOutcome, ApiError> {
        segment::run_segmented_download(
            &self.client,
            self.max_redirects,
            job,
            cfg,
            store,
            &progress,
        )
        .await
    }
}

impl ProtocolEngine for HttpEngine {
    fn name(&self) -> &'static str {
        "http"
    }

    fn supports(&self, url: &str) -> bool {
        Url::parse(url)
            .map(|u| matches!(u.scheme(), "http" | "https"))
            .unwrap_or(false)
    }

    fn download(
        &self,
        job: peregrine_api::DownloadJob,
        progress: peregrine_api::SharedProgressSink,
    ) -> peregrine_api::DownloadFuture<Result<peregrine_api::DownloadOutcome, ApiError>> {
        let client = self.client.clone();
        let max_redirects = self.max_redirects;
        Box::pin(download::run_download(client, max_redirects, job, progress))
    }

    fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>> {
        let client = self.client.clone();
        let max_redirects = self.max_redirects;
        let url = url.to_string();
        Box::pin(async move {
            let deadline = tokio::time::Instant::now() + PROBE_TIMEOUT;
            let mut current = Url::parse(&url)
                .map_err(|e| ApiError::Network(format!("invalid url {url:?}: {e}")))?;

            for hop in 0..=max_redirects {
                let req = Request::builder()
                    .method(hyper::Method::HEAD)
                    .uri(current.as_str())
                    .body(Full::new(Bytes::new()))
                    .map_err(|e| ApiError::Network(format!("build request: {e}")))?;

                let res = tokio::time::timeout_at(deadline, client.request(req))
                    .await
                    .map_err(|_| {
                        ApiError::Network(format!(
                            "probe exceeded {}s budget at {current}",
                            PROBE_TIMEOUT.as_secs()
                        ))
                    })?
                    .map_err(|e| ApiError::Network(format!("request {current}: {e}")))?;

                let status = res.status();

                if status.is_redirection() {
                    let location = res
                        .headers()
                        .get(LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| {
                            ApiError::Network(format!("{status} without Location: {current}"))
                        })?;
                    // `join` resolves relative Locations against `current`.
                    current = current.join(location).map_err(|e| {
                        ApiError::Network(format!("bad Location {location:?}: {e}"))
                    })?;
                    tracing::debug!(hop, to = %current, "redirect");
                    if hop == max_redirects {
                        return Err(ApiError::TooManyRedirects(current.to_string()));
                    }
                    continue;
                }

                if !status.is_success() {
                    return Err(ApiError::Http {
                        status: status.as_u16(),
                        url: current.to_string(),
                    });
                }

                let headers = res.headers();
                let content_length = headers
                    .get(CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let accept_ranges = headers
                    .get(ACCEPT_RANGES)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("bytes"));
                let etag = headers
                    .get(ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                // A weak validator (`W/…`) cannot back `If-Range` (RFC 7233):
                // resuming against one fetches the full body and would glue it
                // onto the partial file. Track strength at the source.
                let etag_strong = etag
                    .as_deref()
                    .is_some_and(|t| !t.trim_start().starts_with("W/"));
                let last_modified = headers
                    .get(LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let filename = content_disposition_filename(headers);

                let mut info = ProbeInfo {
                    url: current.to_string(),
                    content_length,
                    accept_ranges,
                    etag,
                    etag_strong,
                    last_modified,
                    filename,
                };
                // Round 2 (B7): confirm resumability with a real ranged GET —
                // HEAD claims are not trusted on their own.
                confirm_range_support(&client, &mut info, deadline).await;
                return Ok(info);
            }

            // The loop returns on hop == max (redirect branch) or earlier.
            unreachable!("redirect budget is enforced inside the loop")
        })
    }
}

/// B7: probe the server with `GET Range: bytes=0-0` and settle
/// `accept_ranges` by observed behaviour, not advertised headers.
///
/// - `206 Partial Content` → the server really honors `Range`. The total
///   size from `Content-Range` is authoritative and overrides a
///   HEAD-sourced `Content-Length` when they disagree (some origins lie
///   about length on HEAD).
/// - `200 OK` → the server ignored the range → downgrade to
///   single-segment, even if HEAD advertised `Accept-Ranges: bytes`.
/// - Anything else (redirect mid-confirm, 416, 5xx…) → keep the HEAD
///   answer: the planner treats `accept_ranges` optimistically only when
///   the server never confirmed; a failed confirmation is not proof of
///   absence.
///
/// Best-effort by design: probe already succeeded; a broken confirm must
/// not turn a usable `ProbeInfo` into an error.
async fn confirm_range_support(
    client: &HttpsClient,
    info: &mut ProbeInfo,
    deadline: tokio::time::Instant,
) {
    let req = Request::builder()
        .method(hyper::Method::GET)
        .uri(&info.url)
        .header(RANGE, "bytes=0-0")
        .body(Full::new(Bytes::new()));
    let req = match req {
        Ok(req) => req,
        Err(e) => {
            tracing::debug!(error = %e, "range-confirm request build failed");
            return;
        }
    };

    let res = match tokio::time::timeout_at(deadline, client.request(req)).await {
        Ok(Ok(res)) => res,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "range-confirm request failed");
            return;
        }
        Err(_) => {
            tracing::debug!("range-confirm exceeded probe budget");
            return;
        }
    };

    // Decide on status BEFORE touching the body: a server that ignores
    // Range answers 200 with the FULL resource — reading that body would
    // buffer the entire file into memory (P0). Dropping the response closes
    // the pooled connection: the cheap price for not reading it.
    let status = res.status();
    let range_total = content_range_total(res.headers());

    match status.as_u16() {
        206 => {
            // We asked for one byte: drain it so the pooled connection
            // stays reusable. Bounded to 1 byte by the request itself.
            if let Err(e) = http_body_util::BodyExt::collect(res.into_body()).await {
                tracing::debug!(error = %e, "range-confirm body drain failed");
                return;
            }
            info.accept_ranges = true;
            if let Some(total) = range_total {
                if info.content_length != Some(total) {
                    tracing::debug!(
                        head = ?info.content_length,
                        content_range = total,
                        "Content-Range total overrides HEAD Content-Length"
                    );
                }
                info.content_length = Some(total);
            }
        }
        200 => {
            // Server ignored Range: it is sending the full body — drop the
            // response without reading it. Probe already has everything it
            // needs from the HEAD round.
            info.accept_ranges = false;
        }
        other => {
            tracing::debug!(
                status = other,
                "range-confirm inconclusive, keeping HEAD answer"
            );
        }
    }
}

/// Parse the total size out of a `Content-Range` header value, e.g.
/// `bytes 0-0/98765` → `Some(98765)`. An unsized total (`bytes 0-0/*`) or a
/// malformed value yields `None`.
fn content_range_total(headers: &hyper::HeaderMap) -> Option<u64> {
    let raw = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    let total = raw.rsplit('/').next()?.trim();
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

/// Extract `filename` from a `Content-Disposition` header (RFC 6266),
/// handling the quoted (`filename="a.bin"`) and bare (`filename=a.bin`)
/// parameter forms. `filename*` (RFC 5987) is not parsed yet — segment
/// planning does not depend on it.
///
/// The result is sanitized ([`sanitize_filename`]): this is a
/// server-controlled string, and `ProbeInfo` feeds task naming — path
/// components and traversal fragments must never survive here.
fn content_disposition_filename(headers: &hyper::HeaderMap) -> Option<String> {
    let raw = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    for param in raw.split(';').skip(1) {
        // A parameter without `=` (e.g. a stray token) must not abort the
        // scan — later params may still carry the filename.
        let Some((key, value)) = param.trim().split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("filename") {
            continue;
        }
        let value = value.trim();
        let value = if let Some(stripped) = value.strip_prefix('"') {
            stripped.strip_suffix('"')?
        } else {
            value
        };
        return sanitize_filename(value);
    }
    None
}

/// Reduce a server-supplied filename to a bare name: strip any path
/// components (`/` and `\\`, Windows-style included) and refuse traversal
/// fragments, empty results, and NUL bytes. `None` = no usable name.
fn sanitize_filename(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?.trim();
    if base.is_empty() || base == "." || base == ".." || base.contains('\0') {
        return None;
    }
    Some(base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(value: &str) -> hyper::HeaderMap {
        let mut m = hyper::HeaderMap::new();
        m.insert(
            hyper::header::CONTENT_DISPOSITION,
            hyper::header::HeaderValue::from_str(value).unwrap(),
        );
        m
    }

    #[test]
    fn filename_quoted_and_bare_forms() {
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=\"a.bin\"")),
            Some("a.bin".into())
        );
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=a.bin")),
            Some("a.bin".into())
        );
        // Case-insensitive parameter name, extra params around it.
        assert_eq!(
            content_disposition_filename(&hdrs(
                "attachment; size=5; FILENAME=\"b.tar.gz\"; mode=read"
            )),
            Some("b.tar.gz".into())
        );
    }

    #[test]
    fn filename_missing_or_malformed_is_none() {
        assert_eq!(content_disposition_filename(&hdrs("attachment")), None);
        assert_eq!(content_disposition_filename(&hdrs("inline; size=5")), None);
        // Known limitation (BACKLOG): a semicolon inside quotes splits the
        // value and the closing quote no longer matches — dropped, not
        // mis-parsed.
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=\"a;b.bin\"")),
            None
        );
    }

    #[test]
    fn filename_is_sanitized_against_path_injection() {
        // Path components are stripped to the final segment.
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=\"../../.bashrc\"")),
            Some(".bashrc".into())
        );
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=/abs/path/x.zip")),
            Some("x.zip".into())
        );
        // Windows separators too.
        assert_eq!(
            sanitize_filename("C:\\tmp\\evil.exe"),
            Some("evil.exe".into())
        );
        // Bare traversal / emptiness is refused outright.
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename("."), None);
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("   "), None);
        assert_eq!(sanitize_filename("a\u{0}b"), None);
    }

    #[test]
    fn content_range_totals() {
        let mut m = hyper::HeaderMap::new();
        m.insert(
            hyper::header::CONTENT_RANGE,
            hyper::header::HeaderValue::from_static("bytes 0-0/98765"),
        );
        assert_eq!(content_range_total(&m), Some(98765));

        m.insert(
            hyper::header::CONTENT_RANGE,
            hyper::header::HeaderValue::from_static("bytes 0-0/*"),
        );
        assert_eq!(content_range_total(&m), None);

        m.insert(
            hyper::header::CONTENT_RANGE,
            hyper::header::HeaderValue::from_static("garbage"),
        );
        assert_eq!(content_range_total(&m), None);
    }

    #[test]
    fn download_content_range_start_and_total() {
        let range = |v: &str| -> hyper::HeaderMap {
            let mut m = hyper::HeaderMap::new();
            m.insert(
                hyper::header::CONTENT_RANGE,
                hyper::header::HeaderValue::from_str(v).unwrap(),
            );
            m
        };
        assert_eq!(
            HttpEngine::parse_content_range(&range("bytes 500-999/1000")),
            Some((500, 999, Some(1000)))
        );
        assert_eq!(
            HttpEngine::parse_content_range(&range("bytes 500-999/*")),
            Some((500, 999, None))
        );
        // Malformed variants all refuse.
        assert_eq!(HttpEngine::parse_content_range(&range("bytes -/")), None);
        assert_eq!(HttpEngine::parse_content_range(&range("items 1-2/3")), None);
        assert_eq!(
            HttpEngine::parse_content_range(&range("bytes x-9/10")),
            None
        );
        // end < start is nonsense — refuse (B20).
        assert_eq!(
            HttpEngine::parse_content_range(&range("bytes 999-500/1000")),
            None
        );
    }
}
