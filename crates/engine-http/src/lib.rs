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

pub(crate) mod auto;
pub(crate) mod download;
pub mod proxy;
pub mod segment;

pub use proxy::{ProxyConfig, ProxyConnector};
pub use segment::{SegmentConfig, plan_ranges};

use http_body_util::Full;
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED,
    LOCATION, RANGE,
};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use peregrine_api::engine::{ProbeFuture, ProbeInfo};
use peregrine_api::{ApiError, ProtocolEngine};
use std::time::Duration;
use url::Url;

/// Whole probe chain budget (HEAD redirect chase + range confirm, all
/// hops included — enforced with a single deadline, not per-request).
/// Downloads are long; probes must not be.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Shared client type — `pub` so sibling engines (engine-hls) reuse
/// the SAME stack and pooling policy instead of dragging a second
/// HTTP implementation into the tree (workspace rule: one HTTP
/// stack; the engine IS the HTTP story).
pub type HttpsClient = Client<hyper_rustls::HttpsConnector<ProxyConnector>, Full<Bytes>>;

/// Settings-center override (roadmap item 2): `proxy_url` from
/// PUT /settings lands here. Read at client-BUILD time — the daemon
/// restores it from the settings KV before constructing the engine,
/// so a change takes effect on the next daemon start.
static PROXY_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install a proxy URL (`http://` or `socks5://`), or clear it with
/// an empty string. Boot-time only — see PROXY_OVERRIDE.
pub fn set_proxy_url(raw: &str) {
    let _ = PROXY_OVERRIDE.set(raw.trim().to_string());
}

pub fn proxy_url() -> Option<&'static str> {
    PROXY_OVERRIDE
        .get()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
}

/// TLS roots: system trust store, one shared config for every client
/// build. A single unreadable cert is skipped with a warn — one bad
/// system entry must not take the daemon down.
fn tls_config() -> Result<rustls::ClientConfig, ApiError> {
    let mut roots = rustls::RootCertStore::empty();
    // 0.8 API: CertificateResult carries os_error + individual failures
    // rather than a plain Result — never abort on a partial read.
    let loaded = rustls_native_certs::load_native_certs();
    for e in &loaded.errors {
        tracing::warn!(error = %e, "reading a system cert source failed");
    }
    let mut skipped = 0usize;
    for cert in &loaded.certs {
        if let Err(e) = roots.add(cert.clone()) {
            skipped += 1;
            tracing::debug!(error = %e, "skipping unreadable system cert");
        }
    }
    tracing::debug!(loaded = roots.len(), skipped, "native TLS roots loaded");
    if roots.is_empty() && !loaded.errors.is_empty() {
        return Err(ApiError::Internal(
            "no usable TLS roots found on this system".into(),
        ));
    }
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// Build the standard pooled HTTPS-or-HTTP client, honoring the
/// configured proxy (CONNECT for https targets, absolute-form for
/// plain http; SOCKS5 tunnels) or direct when none is set.
///
/// Also installs the process-wide rustls CryptoProvider (ring) —
/// workspace feature unification links BOTH ring and aws-lc-rs
/// (librqbit → reqwest/rustls has no ring variant), defeating
/// rustls's auto-detection; without an explicit install the first
/// TLS consumer panics. Centralizing it HERE means every binary
/// and test that builds an HTTP client gets the provider for
/// free — no per-entrypoint discipline to forget.
pub fn https_client() -> Result<HttpsClient, ApiError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let proxy = match proxy_url() {
        None => None,
        Some(raw) => ProxyConfig::parse(raw)?,
    };
    let connector = hyper_rustls::HttpsConnector::from((ProxyConnector::new(proxy), tls_config()?));
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

/// Identify ourselves on every request (probe, single, segments).
/// Real-world smoke finding (M2-d R3): mirrors like tuna 403 a
/// request with no User-Agent — anti-scraping default. A downloader
/// MUST announce itself; same policy as aria2/curl.
pub const USER_AGENT: &str = concat!("peregrine/", env!("CARGO_PKG_VERSION"));

/// Settings-center override (GUI-verify batch-3): a custom User-Agent
/// set via PUT /settings lands here; every request header reads the
/// accessor instead of the const. Empty/unset = the peregrine default.
static USER_AGENT_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install a custom User-Agent (boot restore or a live settings
/// write). Later calls win; requests in flight keep their header.
pub fn set_user_agent(ua: &str) {
    let ua = ua.trim();
    if ua.is_empty() {
        let _ = USER_AGENT_OVERRIDE.set(USER_AGENT.to_string());
    } else {
        let _ = USER_AGENT_OVERRIDE.set(ua.to_string());
    }
}

pub fn user_agent() -> &'static str {
    USER_AGENT_OVERRIDE
        .get()
        .map(|s| s.as_str())
        .unwrap_or(USER_AGENT)
}

/// An HTTP(S) engine with a shared, pooled client.
pub struct HttpEngine {
    client: HttpsClient,
    max_redirects: usize,
    /// B36 write side: when the daemon wires one in, the FIRST
    /// response's validator (strong etag / Last-Modified) is
    /// persisted per (url, sink) so a later resume can send
    /// `If-Range` and detect a changed remote. `None` in tests and
    /// standalone engine use — behavior degrades to today's.
    validator_store: Option<peregrine_storage::Store>,
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
        Ok(Self {
            client: https_client()?,
            max_redirects,
            validator_store: None,
        })
    }

    /// Attach the shared store for validator persistence (B36).
    /// Builder-style; the daemon calls this with the same `Store`
    /// handed to `download_auto`.
    pub fn with_validator_store(mut self, store: peregrine_storage::Store) -> Self {
        self.validator_store = Some(store);
        self
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
        cancel: tokio_util::sync::CancellationToken,
        budget: &peregrine_api::budget::BudgetChain,
    ) -> Result<peregrine_api::DownloadOutcome, ApiError> {
        segment::run_segmented_download(
            &self.client,
            self.max_redirects,
            job,
            cfg,
            store,
            &progress,
            cancel,
            budget,
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
        cancel: tokio_util::sync::CancellationToken,
        budget: &peregrine_api::budget::BudgetChain,
    ) -> peregrine_api::DownloadFuture<Result<peregrine_api::DownloadOutcome, ApiError>> {
        let client = self.client.clone();
        let max_redirects = self.max_redirects;
        let budget = budget.clone(); // owned by the boxed future ('static)
        let vstore = self.validator_store.clone();
        Box::pin(async move {
            download::run_download(
                client,
                max_redirects,
                job,
                progress,
                cancel,
                &budget,
                vstore.as_ref(),
            )
            .await
        })
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
                    .header(hyper::header::USER_AGENT, crate::user_agent())
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
                    let next = current.join(location).map_err(|e| {
                        ApiError::Network(format!("bad Location {location:?}: {e}"))
                    })?;
                    // Same downgrade guard as fetch_get: a probe must
                    // not silently bless a plaintext hop either.
                    if is_https_downgrade(&current, &next) {
                        return Err(ApiError::Network(format!(
                            "refusing https→http downgrade redirect: {current} → {next}"
                        )));
                    }
                    current = next;
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
        .header(hyper::header::USER_AGENT, crate::user_agent())
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

/// A redirect from an https origin to an http target is a downgrade:
/// the request (with its credentials and If-Range validators) would
/// travel in plaintext. Browsers interstitial-warning this; a
/// downloader refuses outright.
pub(crate) fn is_https_downgrade(from: &Url, to: &Url) -> bool {
    from.scheme() == "https" && to.scheme() == "http"
}

/// Extract `filename` from a `Content-Disposition` header (RFC 6266).
///
/// Long-tail hardening (roadmap item 4): the parameter forms
/// historically seen in the wild, all of them:
///
/// * bare token      — `filename=a.bin`
/// * quoted-string   — `filename="a.bin"` with `\\"` / `\\\\` escapes
/// * RFC 5987 ext    — `filename*=UTF-8''%E4%B8%AD.bin` (also
///   `iso-8859-1` and `us-ascii` charsets); takes priority over the
///   plain form per RFC 6266 §4.3 when both are present
///
/// A server-controlled string feeds task naming, so the result is
/// sanitized ([`sanitize_filename`]) — path components, traversal
/// fragments, control characters and Windows reserved names must
/// never survive.
fn content_disposition_filename(headers: &hyper::HeaderMap) -> Option<String> {
    let raw = headers.get(CONTENT_DISPOSITION)?.to_str().ok()?;
    let mut plain: Option<String> = None;
    for param in raw.split(';').skip(1) {
        // A parameter without `=` (e.g. a stray token) must not abort the
        // scan — later params may still carry the filename.
        let Some((key, value)) = param.trim().split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("filename*") {
            // RFC 5987 ext-value: charset'%lang'%percent-encoded.
            // First ext param wins; it also outranks every plain
            // filename= (RFC 6266: "many user agent implementations
            // [...] this field supersedes").
            if let Some(decoded) = decode_ext_value(value) {
                return sanitize_filename(&decoded);
            }
            // A malformed ext param falls through to the plain form
            // rather than poisoning the whole header.
            continue;
        }
        if key.eq_ignore_ascii_case("filename") && plain.is_none() {
            let value = unquote_cd_param(value)?;
            plain = Some(value);
        }
    }
    let plain = plain?;
    sanitize_filename(&plain)
}

/// Strip quotes and unescape a quoted-string CD parameter (`\\"` → `"`,
/// `\\` → `\\`; any other backslash pair keeps the escaped char verbatim —
/// the lenient reading real servers rely on). Bare tokens pass through.
fn unquote_cd_param(value: &str) -> Option<String> {
    if let Some(stripped) = value.strip_prefix('"') {
        let inner = stripped.strip_suffix('"')?;
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(e @ ('"' | '\\')) => out.push(e),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        Some(out)
    } else {
        Some(value.to_string())
    }
}

/// Decode an RFC 5987 `charset'language'percent-encoded` ext-value.
/// Unknown charsets are refused (spec: "none" means the parameter is
/// ignored), not mis-decoded.
fn decode_ext_value(value: &str) -> Option<String> {
    let (charset, rest) = value.split_once('\'')?;
    let encoded = rest.split_once('\'')?.1;
    if encoded.is_empty() {
        return None;
    }
    let bytes: Vec<u8> = percent_decode(encoded)?;
    match charset.to_ascii_lowercase().as_str() {
        "utf-8" | "us-ascii" => String::from_utf8(bytes).ok(),
        "iso-8859-1" => Some(bytes.iter().map(|&b| b as char).collect()),
        _ => None,
    }
}

/// Strict percent-decoding: `%XX` pairs only; a stray `%` poisons the
/// value (defensive — a decoded filename that was garbage anyway must
/// not mask a plain `filename=` sibling).
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Reduce a server-supplied filename to a safe bare name:
///
/// * strip every path component (`/` and `\`, Windows-style included)
/// * refuse traversal fragments, empties and NUL bytes
/// * strip control characters (a terminal rendering the task list must
///   never eat an escape sequence from a remote header)
/// * Windows reserved device names (`CON`, `COM1`…) get a `_` prefix —
///   the same download landing on Windows must not collide with a
///   device the OS refuses to create
/// * trailing dots/spaces stripped (Windows drops them, and a name
///   ending in `.` breaks extension detection)
/// * clamp to 200 chars so a 4 KiB junk header cannot wedge the
///   save-path UI or the filesystem (255-byte component ceiling)
fn sanitize_filename(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?.trim();
    if base.is_empty() || base == "." || base == ".." || base.contains('\0') {
        return None;
    }
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let mut name: String = cleaned.trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() {
        return None;
    }
    if name.chars().count() > 200 {
        name = name.chars().take(200).collect();
    }
    // Windows reserved names: the DEVICE part before any extension.
    let stem: String = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        name = format!("_{name}");
    }
    Some(name)
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
    fn filename_ext_param_decodes_utf8_and_wins() {
        // RFC 5987: UTF-8 percent-encoded ext-value.
        assert_eq!(
            content_disposition_filename(&hdrs(
                "attachment; filename=\"fallback.bin\"; filename*=UTF-8''%E4%B8%AD%E6%96%87.zip"
            )),
            Some("中文.zip".into())
        );
        // Bare ext param (no plain sibling) also works.
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename*=utf-8''a%20b.bin")),
            Some("a b.bin".into())
        );
    }

    #[test]
    fn filename_ext_param_latin1_and_refusals() {
        // iso-8859-1: bytes map to U+0080..U+00FF verbatim.
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename*=ISO-8859-1''caf%E9.bin")),
            Some("café.bin".into())
        );
        // Unknown charset → parameter ignored → plain sibling survives.
        assert_eq!(
            content_disposition_filename(&hdrs(
                "attachment; filename=plain.bin; filename*=gbk''%C4%E3.bin"
            )),
            Some("plain.bin".into())
        );
        // Malformed (stray %) → ignored, plain wins.
        assert_eq!(
            content_disposition_filename(&hdrs(
                "attachment; filename=plain.bin; filename*=UTF-8''%zz"
            )),
            Some("plain.bin".into())
        );
    }

    #[test]
    fn filename_quoted_escapes_unescape() {
        // RFC 6266 quoted-pair, unit level (before sanitizing):
        // \" is a literal quote; a backslash before anything else
        // keeps both characters (the lenient reading real servers
        // rely on).
        assert_eq!(
            unquote_cd_param("\"a\\\"b.bin\"").as_deref(),
            Some("a\"b.bin")
        );
        assert_eq!(
            unquote_cd_param("\"a\\\\b.bin\"").as_deref(),
            Some("a\\b.bin")
        );
        // Through the full header + sanitize, a decoded backslash is a
        // Windows path separator: basename only.
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=\"a\\\\b.bin\"")),
            Some("b.bin".into())
        );
    }

    #[test]
    fn sanitize_strips_traversal_controls_and_windows_traps() {
        // Path components (unix + windows) never survive.
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=\"../../etc/passwd\"")),
            Some("passwd".into())
        );
        assert_eq!(
            content_disposition_filename(&hdrs("attachment; filename=\"C:\\\\evil\\\\x.bin\"")),
            Some("x.bin".into())
        );
        // Control characters are stripped; the rest of the name survives.
        assert_eq!(
            sanitize_filename("in\u{1b}[31mfect.bin").as_deref(),
            Some("in[31mfect.bin")
        );
        // Windows reserved device name gets a prefix (with or without ext).
        assert_eq!(sanitize_filename("CON"), Some("_CON".into()));
        assert_eq!(sanitize_filename("com1.zip"), Some("_com1.zip".into()));
        assert_eq!(sanitize_filename("NUL.tar.gz"), Some("_NUL.tar.gz".into()));
        // Trailing dots/spaces dropped (Windows FS trap).
        assert_eq!(sanitize_filename("a.bin. .."), Some("a.bin".into()));
        // Empty after sanitizing → None.
        assert_eq!(sanitize_filename(".. "), None);
        assert_eq!(sanitize_filename("\u{1}\u{2}"), None);
        // Length clamp at 200 chars.
        let long = "x".repeat(500);
        assert_eq!(
            sanitize_filename(&long).map(|s| s.chars().count()),
            Some(200)
        );
    }

    #[test]
    fn downgrade_redirect_detection() {
        let https = Url::parse("https://a.example/f").unwrap();
        let http = Url::parse("http://a.example/f").unwrap();
        let https2 = Url::parse("https://b.example/f").unwrap();
        assert!(is_https_downgrade(&https, &http));
        assert!(
            !is_https_downgrade(&https, &https2),
            "cross-host https is fine"
        );
        assert!(!is_https_downgrade(&http, &http), "http to http is fine");
        assert!(!is_https_downgrade(&http, &https), "upgrade is fine");
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
