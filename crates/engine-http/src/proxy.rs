//! Proxy connector: HTTP CONNECT tunnels + SOCKS5, behind one
//! `tower::Service<Uri>` the pooled HTTPS client is built on.
//!
//! Roadmap item 2. The engine previously hardwired `HttpConnector`
//! (direct, no proxy). Real-world use — and our own dev machine —
//! sits behind proxies, so the connector layer now routes every
//! connection through an optional [`ProxyConfig`]:
//!
//! * `http://` proxy + `https://` target → **CONNECT** tunnel, TLS
//!   layered on the tunnel by hyper-rustls as usual
//! * `http://` proxy + `http://` target → plain TCP to the proxy;
//!   hyper emits absolute-form request lines for absolute URIs (our
//!   requests are always absolute), which is exactly what an
//!   RFC 7230 §5.3.2 proxy expects
//! * `socks5://` proxy → a hand-written SOCKS5 client handshake
//!   (RFC 1928, optional username/password auth, RFC 1929) — no new
//!   dependency; the workspace ships zero crypto in that path
//!
//! Direct mode (no config) keeps plain TCP with a connect deadline.
//! The dropped frill versus `HttpConnector` is happy-eyeballs; every
//! other property (pooling, keep-alive, TLS stack) is unchanged.

use hyper::Uri;
use hyper_util::client::legacy::connect::Connected;
use hyper_util::client::legacy::connect::Connection as HyperConnection;
use hyper_util::rt::TokioIo;
use peregrine_api::ApiError;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::Service;
use url::Url;

/// CONNECT/socks dial budget. Probes have their own 15 s budget one
/// level up; this only bounds the TCP/proxy handshake itself.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Parsed proxy configuration (settings KV `proxy_url`).
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Proxy origin: scheme (http|socks5), host, port.
    pub origin: Uri,
    /// Optional credentials: Basic auth for CONNECT, RFC 1929
    /// username/password for SOCKS5.
    pub auth: Option<(String, String)>,
}

impl ProxyConfig {
    /// Parse a settings-string proxy URL: `http://[user:pass@]host:port`
    /// or `socks5://[user:pass@]host:port`. The empty string means
    /// "direct" (None).
    pub fn parse(raw: &str) -> Result<Option<Self>, ApiError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let parsed = Url::parse(raw)
            .map_err(|e| ApiError::Network(format!("bad proxy url {raw:?}: {e}")))?;
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "socks5" {
            return Err(ApiError::Network(format!(
                "unsupported proxy scheme {scheme:?} in {raw:?} (use http:// or socks5://)"
            )));
        }
        let host = parsed
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| ApiError::Network(format!("proxy url {raw:?} has no host")))?;
        let port = parsed.port_or_known_default().ok_or_else(|| {
            ApiError::Network(format!("proxy url {raw:?} has no port and none is implied"))
        })?;
        let auth = match (parsed.username(), parsed.password()) {
            ("", None) => None,
            (u, p) => Some((u.to_string(), p.unwrap_or("").to_string())),
        };
        let origin = Uri::builder()
            .scheme(scheme)
            .authority(format!("{host}:{port}"))
            .path_and_query("/")
            .build()
            .map_err(|e| ApiError::Network(format!("proxy origin: {e}")))?;
        Ok(Some(Self { origin, auth }))
    }
}

/// The connector's stream: direct TCP, or a tunnel that already ends
/// at the target (CONNECT or SOCKS5). TLS is layered on top by
/// hyper-rustls for https targets in every case.
///
/// hyper-rustls 0.27 speaks hyper 1.x rt traits (`hyper::rt::Read`
/// / `Write`), not tokio's — the TcpStream is carried inside a
/// `TokioIo`, which already implements them.
pub enum ProxyStream {
    Direct { inner: TokioIo<TcpStream> },
    Tunneled { inner: TokioIo<TcpStream> },
}

impl hyper::rt::Read for ProxyStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Direct { inner } | ProxyStream::Tunneled { inner } => {
                Pin::new(inner).poll_read(cx, buf)
            }
        }
    }
}

impl hyper::rt::Write for ProxyStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            ProxyStream::Direct { inner } | ProxyStream::Tunneled { inner } => {
                Pin::new(inner).poll_write(cx, buf)
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Direct { inner } | ProxyStream::Tunneled { inner } => {
                Pin::new(inner).poll_flush(cx)
            }
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Direct { inner } | ProxyStream::Tunneled { inner } => {
                Pin::new(inner).poll_shutdown(cx)
            }
        }
    }
}

impl HyperConnection for ProxyStream {
    fn connected(&self) -> Connected {
        // No reuse hints, no negotiated h2 — http/1.1 over a fresh
        // dial every time is the honest baseline for tunneled dials.
        Connected::new()
    }
}

/// The tower service hyper's legacy client dials with.
#[derive(Clone)]
pub struct ProxyConnector {
    proxy: Option<ProxyConfig>,
}

impl ProxyConnector {
    pub fn new(proxy: Option<ProxyConfig>) -> Self {
        Self { proxy }
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = ProxyStream;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let proxy = self.proxy.clone();
        Box::pin(async move {
            let host = dst.host().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "target uri has no host")
            })?;
            let port = dst
                .port_u16()
                .unwrap_or(if dst.scheme_str() == Some("https") {
                    443
                } else {
                    80
                });

            match proxy {
                None => {
                    let s = dial((host, port)).await?;
                    Ok(ProxyStream::Direct {
                        inner: TokioIo::new(s),
                    })
                }
                Some(p) if p.origin.scheme_str() == Some("socks5") => {
                    let phost = p.origin.host().unwrap_or_default().to_string();
                    let pport = p.origin.port_u16().unwrap_or(1080);
                    let mut s = dial((phost.as_str(), pport)).await?;
                    socks5_handshake(&mut s, host, port, p.auth.as_ref()).await?;
                    Ok(ProxyStream::Tunneled {
                        inner: TokioIo::new(s),
                    })
                }
                Some(p) => {
                    let phost = p.origin.host().unwrap_or_default().to_string();
                    let pport = p.origin.port_u16().unwrap_or(80);
                    let https_target = dst.scheme_str() == Some("https");
                    if https_target {
                        // CONNECT tunnel: the proxy relays raw bytes to
                        // host:port; TLS happens above us.
                        let mut s = dial((phost.as_str(), pport)).await?;
                        connect_tunnel(&mut s, host, port, p.auth.as_ref()).await?;
                        Ok(ProxyStream::Tunneled {
                            inner: TokioIo::new(s),
                        })
                    } else {
                        // Plain http via proxy: connect to the PROXY and
                        // let hyper's absolute-form request line carry
                        // the real target.
                        let s = dial((phost.as_str(), pport)).await?;
                        Ok(ProxyStream::Direct {
                            inner: TokioIo::new(s),
                        })
                    }
                }
            }
        })
    }
}

/// Name-resolution note: hostnames resolve LOCALLY even through the
/// proxy (tokio's lookup_host). A socks5h-style remote-DNS mode is a
/// deliberate non-goal for v1 — local fake-ip DNS is the norm on the
/// machines this daemon targets.
async fn dial(addr: (&str, u16)) -> std::io::Result<TcpStream> {
    let (host, port) = addr;
    let resolved = tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("dns: no addr for {host}"),
            )
        })?;
    tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(resolved))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))?
}

/// Send `CONNECT host:port HTTP/1.1`, require a `200` head, and leave
/// the stream as a raw pipe to the target.
async fn connect_tunnel(
    s: &mut TcpStream,
    host: &str,
    port: u16,
    auth: Option<&(String, String)>,
) -> std::io::Result<()> {
    let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some((user, pass)) = auth {
        use base64::Engine as _;
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        req.push_str(&format!("Proxy-Authorization: Basic {cred}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;
    s.flush().await?;

    // Read the response head (through CRLFCRLF) — bodies are not a
    // thing for interim-free CONNECT answers.
    let mut head = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        let n = tokio::time::timeout(DIAL_TIMEOUT, s.read(&mut byte)).await??;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "proxy closed during CONNECT",
            ));
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
        if head.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "CONNECT response head too large",
            ));
        }
    }
    let head_str = String::from_utf8_lossy(&head);
    let status_ok = head_str
        .split_whitespace()
        .nth(1)
        .is_some_and(|code| code == "200");
    if !status_ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "proxy CONNECT refused: {}",
                head_str.lines().next().unwrap_or("")
            ),
        ));
    }
    Ok(())
}

/// SOCKS5 client handshake (RFC 1928 + RFC 1929 auth), then CONNECT to
/// `host:port`. Domain-name addresses (ATYP=0x03) always — local DNS
/// then proxy-side expectations both resolve this way.
async fn socks5_handshake(
    s: &mut TcpStream,
    host: &str,
    port: u16,
    auth: Option<&(String, String)>,
) -> std::io::Result<()> {
    // Greeting: offer no-auth always, user/pass when credentials exist.
    let methods: &[u8] = if auth.is_some() {
        &[0x00, 0x02]
    } else {
        &[0x00]
    };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    s.write_all(&greeting).await?;

    let mut resp = [0u8; 2];
    read_exact_timeout(s, &mut resp).await?;
    if resp[0] != 0x05 {
        return Err(bad_socks("not a SOCKS5 proxy"));
    }
    match resp[1] {
        0x00 => {
            if auth.is_some() {
                // Server picked no-auth even though we offered
                // credentials — fine, nothing more to send.
            }
        }
        0x02 => {
            let Some((user, pass)) = auth else {
                return Err(bad_socks("proxy demands username/password auth"));
            };
            // RFC 1929: ver 1, ulen, uname, plen, passwd.
            let mut req = vec![0x01, user.len() as u8];
            req.extend_from_slice(user.as_bytes());
            req.push(pass.len() as u8);
            req.extend_from_slice(pass.as_bytes());
            s.write_all(&req).await?;
            let mut verdict = [0u8; 2];
            read_exact_timeout(s, &mut verdict).await?;
            if verdict[1] != 0x00 {
                return Err(bad_socks("socks5 username/password auth rejected"));
            }
        }
        0xFF => return Err(bad_socks("no acceptable SOCKS5 auth method")),
        other => return Err(bad_socks(&format!("unknown socks5 method 0x{other:02x}"))),
    }

    // CONNECT request: VER 5, CMD 1 (CONNECT), RSV 0, ATYP 3 (domain).
    if host.len() > 255 {
        return Err(bad_socks("hostname too long for socks5"));
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;

    // Reply: VER REP RSV ATYP + variable address. Read the fixed head,
    // then drain the bound address so the stream is clean for payload.
    let mut head = [0u8; 4];
    read_exact_timeout(s, &mut head).await?;
    if head[1] != 0x00 {
        return Err(bad_socks(&format!(
            "socks5 CONNECT failed with reply 0x{:02x}",
            head[1]
        )));
    }
    // Every BND.ADDR is followed by BND.PORT (2 bytes) — leaving it
    // unread would leak those bytes into the payload stream (found by
    // the echo mock: payload arrived prefixed with the two port zeros).
    let drain = match head[3] {
        0x01 => 4 + 2,  // IPv4 addr + port
        0x04 => 16 + 2, // IPv6 addr + port
        0x03 => {
            let mut l = [0u8; 1];
            read_exact_timeout(s, &mut l).await?;
            1 + l[0] as usize + 2 // length byte + name + port
        }
        other => return Err(bad_socks(&format!("bad socks5 ATYP 0x{other:02x}"))),
    };
    let mut addr_buf = vec![0u8; drain];
    read_exact_timeout(s, &mut addr_buf).await?;
    Ok(())
}

async fn read_exact_timeout(s: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    tokio::time::timeout(DIAL_TIMEOUT, s.read_exact(buf))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "socks5 handshake timeout"))?
        .map(|_| ())
}

fn bad_socks(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, msg.to_string())
}

/// Re-export for lib.rs's client builder.
pub type BoxedConnectFuture = Pin<Box<dyn Future<Output = Result<ProxyStream, Infallible>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_and_socks5_with_credentials() {
        let p = ProxyConfig::parse("http://127.0.0.1:8080")
            .unwrap()
            .unwrap();
        assert_eq!(p.origin.port_u16(), Some(8080));
        assert!(p.auth.is_none());

        let p = ProxyConfig::parse("socks5://user:pw@10.0.0.1:1080")
            .unwrap()
            .unwrap();
        assert_eq!(p.origin.scheme_str(), Some("socks5"));
        assert_eq!(p.auth, Some(("user".to_string(), "pw".to_string())));

        // Empty = direct.
        assert!(ProxyConfig::parse("   ").unwrap().is_none());
        assert!(ProxyConfig::parse("").unwrap().is_none());
    }

    #[test]
    fn rejects_bad_configs_with_pointed_errors() {
        for bad in [
            "ftp://x:1",
            "http://",
            "socks5://host", // no port, none implied
            "not a url",
        ] {
            assert!(ProxyConfig::parse(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[tokio::test]
    async fn connect_tunnel_talks_http_and_refuses_errors() {
        // A minimal CONNECT proxy: read one request head, answer 200,
        // then echo. First run proves the tunnel handshake + payload;
        // a second proxy answers 403 and the handshake must fail.
        async fn run_proxy(reply: &'static str) -> std::net::SocketAddr {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move {
                let (mut sock, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 512];
                let n = sock.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1"), "{req}");
                sock.write_all(reply.as_bytes()).await.unwrap();
                if reply.starts_with("HTTP/1.1 200") {
                    // Tunnel established: echo server side.
                    let (mut r, mut w) = tokio::io::split(sock);
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                }
            });
            addr
        }

        let addr = run_proxy("HTTP/1.1 200 Connection Established\r\n\r\n").await;
        let mut s = dial((addr.ip().to_string().as_str(), addr.port()))
            .await
            .unwrap();
        connect_tunnel(&mut s, "example.com", 443, None)
            .await
            .expect("200 must pass");
        // Payload flows through the same stream after the head.
        s.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        let addr = run_proxy("HTTP/1.1 403 Forbidden\r\n\r\n").await;
        let mut s = dial((addr.ip().to_string().as_str(), addr.port()))
            .await
            .unwrap();
        let err = connect_tunnel(&mut s, "example.com", 443, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("403"), "{err}");
    }

    #[tokio::test]
    async fn socks5_handshake_completes_against_minimal_server() {
        // Minimal SOCKS5 server: offer no-auth, then CONNECT ok, then
        // echo. Exercises greeting/auth-negotiation/reply parsing.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 32];
            let n = sock.read(&mut buf).await.unwrap();
            // greeting: 05 01 00
            assert_eq!(&buf[..3], &[0x05, 0x01, 0x00]);
            let _ = n;
            sock.write_all(&[0x05, 0x00]).await.unwrap();
            // connect req: 05 01 00 03 len host port(2) — read it all.
            let mut head = [0u8; 5];
            sock.read_exact(&mut head).await.unwrap();
            let hlen = head[4] as usize;
            let mut rest = vec![0u8; hlen + 2];
            sock.read_exact(&mut rest).await.unwrap();
            // bound address: ATYP 1, 4 bytes, port 2.
            sock.write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            let (mut r, mut w) = tokio::io::split(sock);
            let _ = tokio::io::copy(&mut r, &mut w).await;
        });

        let mut s = dial((addr.ip().to_string().as_str(), addr.port()))
            .await
            .unwrap();
        socks5_handshake(&mut s, "example.com", 443, None)
            .await
            .expect("handshake must succeed");
        s.write_all(b"pong").await.unwrap();
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn socks5_userpass_path_negotiates() {
        // Server offers 02; verifies RFC 1929 credentials, then CONNECT.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 32];
            // Greeting with credentials offered: VER NMETHODS=2 [00 02]
            sock.read_exact(&mut buf[..4]).await.unwrap();
            assert_eq!(&buf[..4], &[0x05, 0x02, 0x00, 0x02]);
            sock.write_all(&[0x05, 0x02]).await.unwrap(); // demand auth
            // RFC 1929 subnegotiation: ver ulen uname plen passwd
            let mut u = [0u8; 2];
            sock.read_exact(&mut u).await.unwrap();
            assert_eq!(u, [0x01, 4]); // ver 1, ulen 4
            let mut name = [0u8; 4];
            sock.read_exact(&mut name).await.unwrap();
            assert_eq!(&name, b"user");
            let mut p = [0u8; 1];
            sock.read_exact(&mut p).await.unwrap(); // plen
            assert_eq!(p[0], 6);
            let mut pass = vec![0u8; p[0] as usize];
            sock.read_exact(&mut pass).await.unwrap();
            assert_eq!(pass, b"secret".to_vec());
            sock.write_all(&[0x01, 0x00]).await.unwrap(); // auth ok
            let mut head = [0u8; 5];
            sock.read_exact(&mut head).await.unwrap(); // connect req head
            let mut rest = vec![0u8; head[4] as usize + 2];
            sock.read_exact(&mut rest).await.unwrap();
            sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });

        let mut s = dial((addr.ip().to_string().as_str(), addr.port()))
            .await
            .unwrap();
        socks5_handshake(
            &mut s,
            "example.com",
            443,
            Some(&("user".into(), "secret".into())),
        )
        .await
        .expect("auth handshake must succeed");
    }
}
