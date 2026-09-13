//! `DownloadPort` over `HlsEngine::download_merge` (PROPOSAL §4:
//! engine-hls reuses the HTTP worker policy). Mirrors `HttpAutoPort`:
//! budget wiring in the port, protocol in the engine. v1 deltas
//! (tracked in BACKLOG): no per-task segment telemetry (the wire
//! `SegmentView` is byte-range shaped — B43), per-task throttle is
//! a no-op beyond the global chain (B44), `job.resume` ignored
//! (parts-dir idempotence IS the resume).

use crate::DownloadPort;
use peregrine_api::ApiError;
use peregrine_api::budget::BudgetChain;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_engine_hls::HlsEngine;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub struct HlsAutoPort {
    engine: HlsEngine,
    global: peregrine_api::budget::SharedRateBudget,
}

impl HlsAutoPort {
    pub fn new(global: peregrine_api::budget::SharedRateBudget) -> Result<Self, ApiError> {
        Ok(Self {
            engine: HlsEngine::new()?,
            global,
        })
    }
}

impl DownloadPort for HlsAutoPort {
    fn auto_download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        // No per-task local bucket for HLS v1: BudgetChain with an
        // unlimited local still consults the GLOBAL budget, so
        // daemon-wide throttling and fairness hold (B44 for per-task).
        let budget = BudgetChain {
            local: peregrine_api::budget::RateBudget::unlimited(),
            global: self.global.clone(),
        };
        Box::pin(async move {
            let out = self
                .engine
                .download_merge(&job, progress, cancel, &budget)
                .await
                .map_err(|e| {
                    // The engine's cancel marker maps to the port
                    // contract: pause/shutdown, never a task failure.
                    if matches!(
                        &e,
                        peregrine_engine_hls::error::HlsError::Network(m) if m == "cancelled"
                    ) {
                        ApiError::Cancelled
                    } else {
                        ApiError::from(e)
                    }
                })?;
            Ok(out)
        })
    }

    fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let _ = url;
        let dir = {
            let mut s = sink.as_os_str().to_os_string();
            s.push(".parts");
            std::path::PathBuf::from(s)
        };
        Box::pin(async move {
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(anyhow::anyhow!("purge {dir:?}: {e}")),
            }
        })
    }

    fn set_task_limit(&self, _url: &str, _sink: &std::path::Path, _bps: Option<u64>) {
        // B44: per-task throttle on HLS arrives with a per-task
        // budget registry like HttpAutoPort's. Global still applies.
    }
}
