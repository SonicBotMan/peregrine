//! `peregrined` — the headless daemon.
//!
//! Composition (M2-d): `Daemon::build` wires store → task manager →
//! scheduler → HTTP engine; this binary owns only the OS boundary —
//! socket lifecycle, signals, tracing — plus the one ordering rule
//! that matters: the scheduler loop starts only after crash recovery
//! (`start()`), and stops before the socket file is removed.

mod cli;

use anyhow::Context;
use clap::Parser;
use peregrine_api::transport::socket_path;
use peregrine_engine_http::SegmentConfig;
use peregrine_scheduler::SchedulerConfig;
use peregrine_server::{daemon::Daemon, uds};

use crate::cli::Args;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Bind FIRST, start SECOND (P0 from R2 review): a second
    // `peregrined` against the same socket must fail BEFORE
    // `daemon.start()` runs crash recovery against a db another
    // daemon owns, or before its scheduler can double-claim tasks
    // (the busy-path guard is per-process and cannot cross the
    // process boundary).
    let path = socket_path(args.socket.as_deref())?;
    let (listener, socket_id) = uds::bind(&path).await?;

    // Wire the daemon, then start it: crash recovery re-queues
    // interrupted tasks BEFORE the loop can claim anything. Socket
    // ownership is already proven — this instance is THE daemon.
    let daemon = std::sync::Arc::new(Daemon::build(
        args.db.as_deref(),
        SchedulerConfig::default(),
        SegmentConfig::default(),
    )?);
    let requeued = daemon.start().await?;
    if requeued > 0 {
        tracing::info!(
            count = requeued,
            "crash recovery: interrupted tasks requeued"
        );
    }

    let app = daemon.router();

    tracing::info!(
        path = %path.display(),
        db = %daemon.db_path.display(),
        version = peregrine_api::VERSION,
        "peregrined listening"
    );

    // Clean shutdown removes the socket file; ^C is the normal exit path.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server run loop")?;

    // Drain engines before the socket goes: a client that reconnects
    // after this point must find either nothing or a clean next boot,
    // never a daemon still writing files.
    daemon.sched.shutdown().await;
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
