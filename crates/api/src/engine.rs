//! Protocol engine trait — architecture rule #2: "protocol = trait".
//!
//! A new protocol (HTTP, BT, HLS, FTP, …) is a new crate implementing
//! [`ProtocolEngine`] and registering itself. Zero intrusion into the core.

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
    /// Server honors `Range` requests → segmentation is possible.
    pub accept_ranges: bool,
    /// Entity tag for resumption validation.
    pub etag: Option<String>,
}

/// A protocol engine. Async surface will grow in M1 (download/segment APIs);
/// `probe` is the minimal contract every engine can already honor.
///
/// Engines are registered in a registry (M1) and addressed by [`Self::name`].
pub trait ProtocolEngine: Send + Sync {
    /// Unique engine name, e.g. `"http"`, `"bt"`, `"hls"`.
    fn name(&self) -> &'static str;

    /// Returns true if this engine claims the given URL scheme.
    fn supports(&self, url: &str) -> bool;

    /// Inspect a URL without downloading the body.
    fn probe(&self, url: &str) -> ProbeFuture<Result<ProbeInfo, ApiError>>;
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
