//! `DownloadPort` over `BtEngine` (M4-b2). Same split as
//! `FtpAutoPort`: the port owns assembly concerns, the engine owns
//! protocol. v1 deltas (BACKLOG): no budget wiring (BT throttle
//! needs librqbit's limits API, B44-pattern follow-up); `purge`
//! removes the session entry + files (engine re-verifies on
//! re-add, so there is no separate resume state to drop).

use crate::DownloadPort;
use peregrine_api::ApiError;
use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_engine_bt::BtEngine;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub struct BtAutoPort {
    engine: BtEngine,
}

impl BtAutoPort {
    pub fn new() -> Self {
        Self {
            engine: BtEngine::new(),
        }
    }

    /// B59: production wiring — persist the url→data-folder side
    /// table beside the task DB so cross-restart purges can locate
    /// `.torrent`-sourced data directories.
    pub fn with_registry_persistence(path: std::path::PathBuf) -> Self {
        Self {
            engine: BtEngine::new().with_registry_persistence(path),
        }
    }
}

impl Default for BtAutoPort {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadPort for BtAutoPort {
    fn auto_download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        Box::pin(async move { self.engine.download(job, progress, cancel).await })
    }

    fn purge(
        &self,
        url: &str,
        sink: &Path,
        purge_files: bool,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let engine = &self.engine;
        let url = url.to_string();
        let sink = sink.to_path_buf();
        // M5.1 P0-2: `purge_files` now flows end-to-end from
        // REST/CLI/MCP. The engine still refuses to delete data
        // while sibling tasks hold the torrent (refcount).
        Box::pin(async move { engine.purge(&url, &sink, purge_files).await })
    }

    fn bt_peers(&self, url: &str) -> Option<peregrine_engine_bt::BtPeersSnapshot> {
        self.engine.peers(url)
    }
}
