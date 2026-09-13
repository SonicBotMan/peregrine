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

    /// POST a JSON body, expect a JSON body back (task CRUD).
    pub async fn request_json<Req: serde::Serialize, Res: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&Req>,
    ) -> anyhow::Result<Res> {
        let uri = format!("http://{DAEMON_AUTHORITY}{path}");
        let mut builder = hyper::Request::builder().method(method).uri(uri);
        let full = match body {
            Some(v) => {
                builder = builder.header("content-type", "application/json");
                Full::new(Bytes::from(serde_json::to_vec(v)?))
            }
            None => Full::new(Bytes::new()),
        };
        let req = builder.body(full).context("build request")?;
        let (status, bytes) = self.roundtrip(req).await?;
        anyhow::ensure!(
            status.is_success(),
            "daemon returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).context("parse daemon response")
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let req = Request::get(format!("http://{DAEMON_AUTHORITY}{path}"))
            .body(Full::new(Bytes::new()))
            .context("build request")?;
        let (status, bytes) = self.roundtrip(req).await?;
        anyhow::ensure!(
            status.is_success(),
            "daemon returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).context("parse daemon response")
    }

    /// One hard deadline around request AND body collection — a
    /// daemon that stalls mid-body must fail fast too.
    async fn roundtrip(
        &self,
        req: Request<Full<Bytes>>,
    ) -> anyhow::Result<(hyper::StatusCode, Bytes)> {
        match tokio::time::timeout(REQUEST_TIMEOUT, async {
            let res = self
                .http
                .request(req)
                .await
                .context("request daemon over unix socket (is peregrined running?)")?;
            let status = res.status();
            let bytes = res
                .into_body()
                .collect()
                .await
                .context("read response")?
                .to_bytes();
            Ok::<_, anyhow::Error>((status, bytes))
        })
        .await
        {
            Ok(inner) => inner,
            Err(_elapsed) => anyhow::bail!("request timed out (daemon hung?)"),
        }
    }
}
