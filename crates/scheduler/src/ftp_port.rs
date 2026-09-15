//! `DownloadPort` over `FtpEngine` (M4-c). The port owns budget
//! wiring, the engine owns protocol — same split as `HttpAutoPort`.
//! v1 deltas (BACKLOG): no per-task throttle beyond the global chain
//! (B44 pattern), FTP has no validator to hand to the store
//! (`final_validator: None` — resume authority is SIZE, checked in
//! the engine), single connection per transfer (no segmentation).

use crate::DownloadPort;
use crate::TaskBudgets;
use peregrine_api::ApiError;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_api::engine::ProtocolEngine;
use peregrine_engine_ftp::FtpEngine;
use peregrine_storage::Store;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub struct FtpAutoPort {
    engine: FtpEngine,
    store: Store,
    budgets: TaskBudgets,
}

impl FtpAutoPort {
    pub fn new(global: peregrine_api::budget::SharedRateBudget, store: Store) -> Self {
        Self {
            engine: FtpEngine::new(),
            store,
            budgets: TaskBudgets::new(global),
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
        // QA-E2E Bug 2: per-task LOCAL bucket seeded from the row;
        // the engine pays the chain per slice (lib.rs acquire).
        let (url, sink) = (job.url.clone(), job.sink.clone());
        let store = &self.store;
        let budgets = &self.budgets;
        let engine = &self.engine;
        Box::pin(async move {
            let seed = TaskBudgets::row_seed(store, &url, &sink).await;
            let budget = budgets.budget_for(&url, sink.as_path(), seed);
            let out = engine.download(job, progress, cancel, &budget).await;
            budgets.drop_key(&url, &sink);
            out
        })
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

    fn set_task_limit(&self, url: &str, sink: &Path, bps: Option<u64>) {
        // QA-E2E Bug 2: live poke into the registered local bucket;
        // queued tasks read the row when they start (row_seed).
        self.budgets.poke(url, sink, bps);
    }
}
