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
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        let engine = &self.engine;
        let url = url.to_string();
        let sink = sink.to_path_buf();
        // R2' P1 (M5.1 P0-2 alignment): engine purge here means
        // "drop engine-side state", NOT "delete user data" — data
        // deletion is the caller's explicit choice and rides the
        // purge=true path being threaded through REST/CLI/MCP in
        // M5.1. Until then a plain remove keeps every byte.
        Box::pin(async move { engine.purge(&url, &sink, false).await })
    }
}
