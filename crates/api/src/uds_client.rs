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

/// Where the daemon lives. UDS is the default (desktop); TCP is
/// the headless/service surface (`peregrined --tcp`, systemd
/// system unit) — one client, both transports (M6-c).
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// Unix domain socket path.
    Unix(PathBuf),
    /// `host:port` TCP authority (daemon binds loopback).
    Tcp(String),
}

impl From<PathBuf> for Endpoint {
    fn from(p: PathBuf) -> Self {
        Endpoint::Unix(p)
    }
}

impl From<&str> for Endpoint {
    fn from(s: &str) -> Self {
        // Infallible contexts (tests, defaults) keep the old shape;
        // malformed specs become a Unix path — `parse_checked` is
        // the failing variant production code uses.
        Endpoint::parse_infallible(s)
    }
}

impl Endpoint {
    /// Human-readable form for logs (UDS path or `tcp:authority`).
    pub fn display(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Endpoint::Unix(p) => std::borrow::Cow::Owned(p.display().to_string()),
            Endpoint::Tcp(a) => std::borrow::Cow::Owned(format!("tcp:{a}")),
        }
    }

    /// Parse a `--socket`-style spec (M6-c):
    /// - `tcp:8800` — bare port → `127.0.0.1:8800`
    /// - `tcp:HOST:PORT` / `tcp:[::1]:PORT` — explicit
    /// - anything else — Unix domain socket path
    ///
    /// Fails on specs that LOOK like TCP but are unusable (`tcp:`
    /// empty, `tcp:host` with no port) — failing here beats a
    /// cryptic connect-time error.
    pub fn parse_checked(spec: &str) -> anyhow::Result<Self> {
        let Some(rest) = spec.strip_prefix("tcp:") else {
            return Ok(Endpoint::Unix(PathBuf::from(spec)));
        };
        if rest.is_empty() {
            anyhow::bail!("tcp endpoint needs a port: tcp:PORT or tcp:HOST:PORT");
        }
        // Unbracketed IPv6 ("tcp:::1:8899") is ambiguous — rsplit
        // would mis-parse the colons. Require the bracketed form
        // ("tcp:[::1]:8899"); an authority with ≥2 colons that
        // doesn't start with '[' can't be host:port.
        if rest.matches(':').count() > 1 && !rest.starts_with('[') {
            anyhow::bail!("unbracketed IPv6 — use tcp:[::1]:PORT (got {spec:?})");
        }
        // Bare digits → port on loopback. Validate range here (0 is
        // reserved, >65535 impossible) instead of a cryptic connect
        // error later; the server side rejects 0 too.
        if rest.chars().all(|c| c.is_ascii_digit()) {
            let port: u16 = rest
                .parse()
                .map_err(|_| anyhow::anyhow!("port out of range: {rest}"))?;
            if port == 0 {
                anyhow::bail!("port 0 is not valid for a client endpoint");
            }
            return Ok(Endpoint::Tcp(format!("127.0.0.1:{rest}")));
        }
        // host:port (bracketed IPv6 included) — validate the port
        // shape/range by asking the parser, but keep the string (no
        // resolution here).
        if rest.parse::<std::net::SocketAddr>().is_ok() {
            return Ok(Endpoint::Tcp(rest.to_string()));
        }
        if let Some((_, port)) = rest.rsplit_once(':') {
            let port: u16 = port.parse().unwrap_or(0);
            if port != 0 {
                return Ok(Endpoint::Tcp(rest.to_string()));
            }
        }
        anyhow::bail!("tcp endpoint must be tcp:PORT or tcp:HOST:PORT (got {spec:?})")
    }

    /// Infallible parse for `From<&str>`: malformed TCP specs fall
    /// back to a Unix path (the pre-M6-c behavior).
    pub fn parse_infallible(spec: &str) -> Self {
        Self::parse_checked(spec).unwrap_or_else(|_| Endpoint::Unix(PathBuf::from(spec)))
    }
}

/// Named connector: a `Service<Uri>` that ignores the URI and dials
/// the daemon — UDS path or TCP authority. Plain struct, not
/// type-erased, so it is `Clone + Send + Sync` exactly as
/// hyper-util's legacy client requires. Both stream types are
/// boxed into one `TokioIo`-wrapped connection type.
#[derive(Clone)]
struct DaemonConnector {
    endpoint: Endpoint,
}

/// One connection type for both transports: tokio streams boxed as
/// plain async read+write. (hyper's legacy client needs a single
/// concrete `Connection` type per connector.)
/// Composite supertrait (E0225: trait objects take ONE principal
/// trait; auto traits ride along).
pub trait BoxedIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> BoxedIo for T {}

/// Local wrapper (orphan rule: can't impl foreign Connection for
/// foreign TokioIo<...>) giving hyper ONE concrete connection type
/// for both UDS and TCP transports: TokioIo adapts tokio↔hyper IO,
/// this wrapper adds the hyper-util Connection impl.
struct BoxedStream(TokioIo<Box<dyn BoxedIo>>);

impl hyper::rt::Read for BoxedStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        hyper::rt::Read::poll_read(std::pin::Pin::new(&mut self.get_mut().0), cx, buf)
    }
}

impl hyper::rt::Write for BoxedStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        hyper::rt::Write::poll_write(std::pin::Pin::new(&mut self.get_mut().0), cx, buf)
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        hyper::rt::Write::poll_flush(std::pin::Pin::new(&mut self.get_mut().0), cx)
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        hyper::rt::Write::poll_shutdown(std::pin::Pin::new(&mut self.get_mut().0), cx)
    }
}

impl hyper_util::client::legacy::connect::Connection for BoxedStream {
    fn connected(&self) -> hyper_util::client::legacy::connect::Connected {
        hyper_util::client::legacy::connect::Connected::new()
    }
}

type BoxedConnect = Pin<Box<dyn Future<Output = std::io::Result<BoxedStream>> + Send>>;

impl Service<hyper::Uri> for DaemonConnector {
    type Response = BoxedStream;
    type Error = std::io::Error;
    type Future = BoxedConnect;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        // Nothing to buffer: each call dials fresh.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _uri: hyper::Uri) -> Self::Future {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let stream = match endpoint {
                Endpoint::Unix(socket) => {
                    let stream = tokio::net::UnixStream::connect(socket).await?;
                    BoxedStream(TokioIo::new(Box::new(stream) as Box<dyn BoxedIo>))
                }
                Endpoint::Tcp(authority) => {
                    let stream = tokio::net::TcpStream::connect(&authority).await?;
                    // hyper's HttpConnector sets NODELAY by default;
                    // our custom dial must too, or loopback ping-pong
                    // inherits Nagle delays (M6-c R2).
                    stream.set_nodelay(true)?;
                    BoxedStream(TokioIo::new(Box::new(stream) as Box<dyn BoxedIo>))
                }
            };
            Ok(stream)
        })
    }
}

pub struct DaemonClient {
    http: Client<DaemonConnector, Full<Bytes>>,
    /// URI host: `peregrine` is the placeholder for UDS (no real
    /// authority); for TCP the REAL authority — the daemon's DNS
    /// rebinding guard rejects non-loopback Host headers, and the
    /// endpoint's own host is loopback by construction.
    authority: String,
}

impl DaemonClient {
    pub fn new(endpoint: impl Into<Endpoint>) -> Self {
        let endpoint = endpoint.into();
        let authority = match &endpoint {
            Endpoint::Unix(_) => DAEMON_AUTHORITY.to_string(),
            Endpoint::Tcp(a) => a.clone(),
        };
        let http: Client<DaemonConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build(DaemonConnector { endpoint });
        Self { http, authority }
    }

    /// GET /health, typed via the shared `HealthInfo`.
    pub async fn ping(&self) -> anyhow::Result<crate::health::HealthInfo> {
        self.get_json("/health").await
    }

    /// POST a JSON body, expect a JSON body back (task CRUD).
    pub async fn request_json<Req: serde::Serialize, Res: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&Req>,
    ) -> anyhow::Result<Res> {
        let uri = format!("http://{}{path}", self.authority);
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
        let req = Request::get(format!("http://{}{path}", self.authority))
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
                .context(match &self.authority {
                    a if a == DAEMON_AUTHORITY => {
                        "request daemon over unix socket (is peregrined running?)".to_string()
                    }
                    a => format!("request daemon over tcp {a} (is peregrined running?)"),
                })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(spec: &str) -> String {
        match Endpoint::parse_checked(spec).expect("valid spec") {
            Endpoint::Tcp(a) => a,
            Endpoint::Unix(p) => panic!("expected Tcp, got Unix({})", p.display()),
        }
    }

    #[test]
    fn bare_port_becomes_loopback() {
        assert_eq!(tcp("tcp:8899"), "127.0.0.1:8899");
        assert_eq!(tcp("tcp:8800"), "127.0.0.1:8800");
    }

    #[test]
    fn explicit_host_port_passthrough() {
        assert_eq!(tcp("tcp:127.0.0.1:8899"), "127.0.0.1:8899");
        assert_eq!(tcp("tcp:localhost:8899"), "localhost:8899");
        // Bracketed IPv6 socket-addr form round-trips.
        assert_eq!(tcp("tcp:[::1]:8899"), "[::1]:8899");
    }

    #[test]
    fn non_tcp_is_unix_path() {
        match Endpoint::parse_checked("/run/peregrine.sock").unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, PathBuf::from("/run/peregrine.sock")),
            Endpoint::Tcp(_) => panic!("expected Unix"),
        }
    }

    #[test]
    fn malformed_tcp_specs_rejected() {
        assert!(Endpoint::parse_checked("tcp:").is_err(), "empty");
        assert!(Endpoint::parse_checked("tcp:host").is_err(), "no port");
        assert!(Endpoint::parse_checked("tcp:host:").is_err(), "empty port");
        // Port range / reserved (M6-c R2): 0 rejected, >65535 rejected.
        assert!(Endpoint::parse_checked("tcp:0").is_err(), "port 0");
        assert!(
            Endpoint::parse_checked("tcp:99999").is_err(),
            "port overflow"
        );
        assert!(
            Endpoint::parse_checked("tcp:host:99999").is_err(),
            "host port overflow"
        );
        // Unbracketed IPv6 rejected — bracketed form required.
        assert!(
            Endpoint::parse_checked("tcp:::1:8899").is_err(),
            "unbracketed ipv6"
        );
    }

    #[test]
    fn infallible_falls_back_to_unix() {
        match Endpoint::parse_infallible("tcp:oops") {
            Endpoint::Unix(_) => {}
            Endpoint::Tcp(a) => panic!("expected fallback, got Tcp({a})"),
        }
    }
}
