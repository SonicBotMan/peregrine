//! `DownloadPort` over `HlsEngine::download_merge` (PROPOSAL §4:
//! engine-hls reuses the HTTP worker policy). Mirrors `HttpAutoPort`:
//! budget wiring in the port, protocol in the engine. v1 deltas
//! (tracked in BACKLOG): no per-task segment telemetry (the wire
//! `SegmentView` is byte-range shaped — B43), per-task throttle is
//! a no-op beyond the global chain (B44), `job.resume` ignored
//! (parts-dir idempotence IS the resume).

use crate::DownloadPort;
use crate::TaskBudgets;
use peregrine_api::ApiError;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_engine_hls::HlsEngine;
use peregrine_storage::Store;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub struct HlsAutoPort {
    engine: HlsEngine,
    store: Store,
    budgets: TaskBudgets,
}

impl HlsAutoPort {
    pub fn new(
        global: peregrine_api::budget::SharedRateBudget,
        store: Store,
    ) -> Result<Self, ApiError> {
        Ok(Self {
            engine: HlsEngine::new()?,
            store,
            budgets: TaskBudgets::new(global),
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
        // QA-E2E Bug 2: per-task LOCAL bucket seeded from the row
        // (same semantics as HttpAutoPort); the engine already pays
        // the chain per slice (fetch.rs). Registered live so
        // `set_task_limit` can poke a running task.
        let (url, sink) = (job.url.clone(), job.sink.clone());
        let store = &self.store;
        let budgets = &self.budgets;
        let engine = &self.engine;
        Box::pin(async move {
            let seed = TaskBudgets::row_seed(store, &url, &sink).await;
            let budget = budgets.budget_for(&url, sink.as_path(), seed);
            let out = engine
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
                });
            // Session over (either arm) — drop the registry entry so
            // a future re-add seeds from the row again.
            budgets.drop_key(&url, &sink);
            out
        })
    }

    fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
        purge_files: bool,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let _ = url;
        // Merged output survives a plain remove (M5.1 P0-2); the
        // .parts dir is downloaded data — also user-visible bytes,
        // so it rides the same flag.
        if !purge_files {
            return Box::pin(async { Ok(()) });
        }
        let dir = {
            let mut s = sink.as_os_str().to_os_string();
            s.push(".parts");
            std::path::PathBuf::from(s)
        };
        let sink = sink.to_path_buf();
        Box::pin(async move {
            // Merged output first: the deliverable IS the data the
            // user asked to delete. Best-effort on the sink if the
            // parts dir is the only remnant (merge failed mid-way).
            if let Err(e) = tokio::fs::remove_file(&sink).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(anyhow::anyhow!("purge {sink:?}: {e}"));
            }
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(anyhow::anyhow!("purge {dir:?}: {e}")),
            }
        })
    }

    fn set_task_limit(&self, url: &str, sink: &std::path::Path, bps: Option<u64>) {
        // QA-E2E Bug 2: live poke into the registered local bucket;
        // queued tasks read the row when they start (row_seed).
        self.budgets.poke(url, sink, bps);
    }

    fn finalize(
        &self,
        _url: &str,
        sink: &std::path::Path,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let sink = sink.to_path_buf();
        Box::pin(async move {
            let bytes = HlsEngine::salvage_merge(&sink)
                .await
                .map_err(|e| anyhow::anyhow!("hls salvage {sink:?}: {e}"))?;
            if bytes > 0 {
                tracing::info!(
                    sink = %sink.display(),
                    bytes,
                    "salvaged cancelled recording (gap-tolerant; encrypted streams \
                     salvage to ciphertext — see BACKLOG)"
                );
            }
            Ok(())
        })
    }
}
