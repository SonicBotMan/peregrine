//! Minimal HTTP-over-Unix-socket client (hyper legacy client + UDS connector).
//!
//! hyper-util's legacy client requires the connector's `Future: Unpin`, so the
//! connect future is boxed (`Pin<Box<dyn Future + Send>>`). The connector is a
//! plain named struct (`Clone + Send + Sync`), so one `Client` (with pooling)
//! is constructed per `DaemonClient` and reused across calls. A hard 5s
//! timeout makes a hung daemon fail fast instead of blocking forever.

use anyhow::Context;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::de::DeserializeOwned;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tower::Service;

/// Placeholder host: with UDS the authority is the socket path, the URI host
/// only exists to satisfy HTTP syntax.
const DAEMON_AUTHORITY: &str = "peregrine";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

type BoxedConnect =
    Pin<Box<dyn Future<Output = std::io::Result<TokioIo<tokio::net::UnixStream>>> + Send>>;

/// Named connector: a `Service<Uri>` that ignores the URI and dials the
/// daemon's unix socket. Plain struct, not type-erased, so it is
/// `Clone + Send + Sync` exactly as hyper-util's legacy client requires.
#[derive(Clone)]
struct UdsConnector {
    socket: PathBuf,
}

impl Service<hyper::Uri> for UdsConnector {
    type Response = TokioIo<tokio::net::UnixStream>;
    type Error = std::io::Error;
    type Future = BoxedConnect;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        // Nothing to buffer: each call dials fresh.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _uri: hyper::Uri) -> Self::Future {
        let socket = self.socket.clone();
        Box::pin(async move {
            let stream = tokio::net::UnixStream::connect(socket).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

pub struct DaemonClient {
    http: Client<UdsConnector, Full<Bytes>>,
}

impl DaemonClient {
    pub fn new(socket: PathBuf) -> Self {
        let http: Client<UdsConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build(UdsConnector { socket });
        Self { http }
    }

    /// GET /health, typed via the shared `HealthInfo`.
    pub async fn ping(&self) -> anyhow::Result<peregrine_api::HealthInfo> {
        self.get_json("/health").await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let uri = format!("http://{DAEMON_AUTHORITY}{path}");
        let req = Request::get(uri)
            .body(Full::new(Bytes::new()))
            .context("build request")?;
        let res = tokio::time::timeout(REQUEST_TIMEOUT, self.http.request(req))
            .await
            .context("request timed out (daemon hung?)")?
            .context("request daemon over unix socket (is peregrined running?)")?;
        let status = res.status();
        let bytes = res
            .into_body()
            .collect()
            .await
            .context("read response")?
            .to_bytes();
        anyhow::ensure!(
            status.is_success(),
            "daemon returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).context("parse daemon response")
    }
}
