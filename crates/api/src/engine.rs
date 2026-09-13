//! Protocol engine trait — architecture rule #2: "protocol = trait".
//!
//! A new protocol (HTTP, BT, HLS, FTP, …) is a new crate implementing
//! [`ProtocolEngine`] and registering itself. Zero intrusion into the core.

use crate::download::{DownloadFuture, DownloadJob, DownloadOutcome, SharedProgressSink};
use crate::error::ApiError;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

/// Boxed future returned by [`ProtocolEngine::probe`].
///
/// Boxed on purpose: the M1 engine registry stores `dyn ProtocolEngine`, and
/// trait objects cannot expose RPITIT methods. Boxing a short probe call is
/// free next to the network round-trip it wraps.
pub type ProbeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a probe (阶段1 of the M1 pipeline) learns about a URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeInfo {
    /// Final URL after redirects.
    pub url: String,
    /// Announced content length in bytes, if the server says so.
    pub content_length: Option<u64>,
    /// Server honors `Range` requests → segmentation is possible. The HTTP
    /// engine CONFIRMS this with a real ranged GET whenever HEAD doesn't
    /// advertise it — servers that claim `bytes` but ignore `Range` are
    /// downgraded to single-segment here.
    pub accept_ranges: bool,
    /// Entity tag for resumption validation. Only a STRONG etag may back
    /// `If-Range` (RFC 7233): a weak tag (`W/…`) cannot prove the resource
    /// is unchanged, and `If-Range` with it returns the FULL body — gluing
    /// that onto a partial file corrupts it silently. Check
    /// [`Self::etag_strong`] before using.
    pub etag: Option<String>,
    /// True iff `etag` is a strong validator (no `W/` prefix).
    #[serde(default)]
    pub etag_strong: bool,
    /// `Last-Modified` header — the standard `If-Range` fallback validator
    /// when no strong etag exists.
    #[serde(default)]
    pub last_modified: Option<String>,
    /// Filename from `Content-Disposition`, if present — lets MCP clients
    /// pre-name tasks before any bytes are downloaded.
    #[serde(default)]
    pub filename: Option<String>,
}

/// A protocol engine. `probe` inspects; `download` executes. Engines that
/// cannot yet download (future FTP/BT crates) inherit the `Unsupported`
/// default and implement it in their own milestone.
pub trait ProtocolEngine: Send + Sync {
    /// Unique engine name, e.g. `"http"`, `"bt"`, `"hls"`.
    fn name(&self) -> &'static str;

    /// Returns true if this engine claims the given URL scheme.
    fn supports(&self, url: &str) -> bool;

    /// Inspect a URL without downloading the body.
    fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>>;

    /// Move bytes into `job.sink`. Single connection, resumable, streaming:
    /// returns `Ok` only when the body ended cleanly (`completed` is
    /// always true in `Ok`); a SHORT body relative to the announced
    /// total is `Err`. Cancellation is cooperative via the token —
    /// the engine flushes, leaves a valid partial on disk for the
    /// next resume, and returns `Err(ApiError::Cancelled)`. (M1's
    /// "future-drop" cancellation is not enough for the segmenter,
    /// whose workers are spawned tasks that survive a dropped joiner.)
    ///
    /// Default: this engine does not implement download yet (M1+ engines
    /// override; returns `ApiError::Internal`). A cancelled token is
    /// still honored so every engine, implemented or not, reports a
    /// pause as `Cancelled` rather than an internal failure (M2-b
    /// R2 P2-5).
    fn download(
        &self,
        _job: DownloadJob,
        _progress: SharedProgressSink,
        cancel: tokio_util::sync::CancellationToken,
        _budget: &crate::budget::BudgetChain,
    ) -> DownloadFuture<Result<DownloadOutcome, ApiError>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(ApiError::Cancelled);
            }
            Err(ApiError::Internal(
                "engine does not implement download".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysEngine;

    impl ProtocolEngine for AlwaysEngine {
        fn name(&self) -> &'static str {
            "always"
        }

        fn supports(&self, _url: &str) -> bool {
            true
        }

        fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>> {
            let url = url.to_string();
            Box::pin(async move {
                Ok(ProbeInfo {
                    url,
                    content_length: None,
                    accept_ranges: false,
                    etag: None,
                    etag_strong: false,
                    last_modified: None,
                    filename: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn probe_returns_info() {
        let e = AlwaysEngine;
        assert!(e.supports("anything://x"));
        let info = e.probe("anything://x").await.expect("probe ok");
        assert_eq!(info.url, "anything://x");
        assert_eq!(e.name(), "always");
    }
}
