//! `DownloadPort` over `FtpEngine` (M4-c). The port owns budget
//! wiring, the engine owns protocol — same split as `HttpAutoPort`.
//! v1 deltas (BACKLOG): no per-task throttle beyond the global chain
//! (B44 pattern), FTP has no validator to hand to the store
//! (`final_validator: None` — resume authority is SIZE, checked in
//! the engine), single connection per transfer (no segmentation).

use crate::DownloadPort;
use peregrine_api::ApiError;
use peregrine_api::budget::BudgetChain;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_api::engine::ProtocolEngine;
use peregrine_engine_ftp::FtpEngine;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub struct FtpAutoPort {
    engine: FtpEngine,
    global: peregrine_api::budget::SharedRateBudget,
}

impl FtpAutoPort {
    pub fn new(global: peregrine_api::budget::SharedRateBudget) -> Self {
        Self {
            engine: FtpEngine::new(),
            global,
        }
    }
}

impl DownloadPort for FtpAutoPort {
    fn auto_download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        // Unlimited LOCAL bucket, live GLOBAL chain: daemon-wide
        // throttle + fairness hold (per-task FTP throttle is B44).
        let budget = BudgetChain {
            local: peregrine_api::budget::RateBudget::unlimited(),
            global: self.global.clone(),
        };
        Box::pin(async move { self.engine.download(job, progress, cancel, &budget).await })
    }

    fn purge(
        &self,
        url: &str,
        sink: &Path,
        purge_files: bool,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let _ = url;
        // FTP has no side files (no parts dir): the sink IS the data.
        // M5.1 P0-2: data deletion is explicit — plain remove keeps
        // the downloaded file.
        let sink = sink.to_path_buf();
        if !purge_files {
            return Box::pin(async { Ok(()) });
        }
        Box::pin(async move {
            match tokio::fs::remove_file(&sink).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(anyhow::anyhow!("purge {sink:?}: {e}")),
            }
        })
    }

    fn set_task_limit(&self, _url: &str, _sink: &Path, _bps: Option<u64>) {
        // B44: per-task throttle lands with the budget registry work.
    }
}
