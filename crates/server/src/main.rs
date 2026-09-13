//! `peregrined` — the headless daemon.
//!
//! M0: axum Router served over a Unix domain socket, `GET /health` only.
//! Architecture rules honored from line one:
//! - default transport = Unix socket (never TCP, never 0.0.0.0)
//! - clients share the api types; the daemon is the only privileged process

mod cli;

use anyhow::Context;
use clap::Parser;
use peregrine_api::transport::socket_path;
use peregrine_server::{health, uds};

use crate::cli::Args;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let path = socket_path(args.socket.as_deref())?;
    let started = std::time::Instant::now();
    let app = health::router(health::Health {
        started,
        pid: std::process::id(),
    });

    let (listener, socket_id) = uds::bind(&path).await?;
    tracing::info!(path = %path.display(), version = peregrine_api::VERSION, "peregrined listening");

    // Clean shutdown removes the socket file; ^C is the normal exit path.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server run loop")?;

    uds::remove_socket_file(&path, socket_id).await;
    tracing::info!("peregrined stopped, socket cleaned");
    Ok(())
}

/// Exit cleanly on SIGINT (^C, interactive) and SIGTERM (systemd/kill).
/// Both paths funnel into axum's graceful shutdown, which drains in-flight
/// requests before `remove_socket_file` runs.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received"),
            _ = sigterm.recv() => tracing::info!("SIGTERM received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown signal received");
}
