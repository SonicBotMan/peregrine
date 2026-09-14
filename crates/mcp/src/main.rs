//! `peregrine-mcp` — MCP server binary (M5).
//!
//! Two transports:
//! - **stdio** (default): for Claude Desktop / Claude Code / any
//!   MCP host that spawns a process and speaks JSON-RPC over
//!   stdin/stdout.
//! - **HTTP** (`--http 127.0.0.1:8801`): streamable-HTTP endpoint
//!   for hosts that connect over the network. Sessions are
//!   per-connection (the factory builds a fresh [`PeregrineMcp`]
//!   per session; each carries its own subscription state).
//!
//! Both point at the same daemon socket; `--socket` overrides the
//! default `~/.peregrine/daemon.sock`, `--events` overrides the
//! daemon WS events URL.

use std::path::PathBuf;

use clap::Parser;
use peregrine_mcp::{DEFAULT_EVENTS_URL, PeregrineMcp};
use rmcp::serve_server;
use rmcp::transport::io::stdio;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

/// Default daemon socket: `$HOME/.peregrine/daemon.sock` (the same
/// default the CLI uses).
fn default_socket() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peregrine/daemon.sock")
}

#[derive(Parser)]
#[command(name = "peregrine-mcp", about = "Peregrine MCP server")]
struct Args {
    /// Daemon UDS socket path
    #[arg(long, default_value_t = default_socket().display().to_string())]
    socket: String,

    /// Daemon WebSocket events URL (for push notifications)
    #[arg(long, default_value = DEFAULT_EVENTS_URL)]
    events: String,

    /// Serve streamable-HTTP on this address instead of stdio
    #[arg(long)]
    http: Option<String>,

    /// Silence tracing output below WARN (stdio mode: logs go to
    /// stderr, never stdout — stdout IS the protocol channel).
    #[arg(long, default_value_t = tracing::Level::INFO)]
    log_level: tracing::Level,
}

fn main() -> anyhow::Result<()> {
    peregrine_scheduler::tls::init_tls();
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_max_level(args.log_level)
        .with_writer(std::io::stderr)
        .init();

    let socket = PathBuf::from(&args.socket);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        if let Some(addr) = args.http {
            serve_http(socket, &args.events, &addr).await
        } else {
            serve_stdio(socket, &args.events).await
        }
    })
}

async fn serve_stdio(socket: PathBuf, events_url: &str) -> anyhow::Result<()> {
    tracing::info!(
        "peregrine-mcp (stdio) starting, daemon socket {}",
        socket.display()
    );
    let service = serve_server(PeregrineMcp::new(socket, events_url), stdio()).await?;
    service.waiting().await?;
    Ok(())
}

async fn serve_http(socket: PathBuf, events_url: &str, addr: &str) -> anyhow::Result<()> {
    use std::sync::Arc;

    tracing::info!("peregrine-mcp (http) listening on {addr}");
    let events_url = events_url.to_string();
    let factory = move || Ok(PeregrineMcp::new(socket.clone(), events_url.clone()));
    let service = StreamableHttpService::new(
        factory,
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
