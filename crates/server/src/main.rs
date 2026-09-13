//! `peregrined` — the headless daemon.
//!
//! Composition (M2-d): `Daemon::build` wires store → task manager →
//! scheduler → HTTP engine; this binary owns only the OS boundary —
//! listener lifecycle, signals, tracing — plus the one ordering rule
//! that matters: **bind FIRST, start SECOND, serve THIRD**:
//!
//! 1. Every listener binds before `daemon.start()` runs, so a second
//!    `peregrined` against the same socket/port fails BEFORE it can
//!    run crash recovery against a db another daemon owns.
//! 2. `start()` (crash recovery) completes before the serve loops
//!    accept connections — no client ever sees a daemon whose
//!    scheduler has not yet re-queued interrupted tasks.
//! 3. On shutdown the serve loops drain, engines drain, and only
//!    then are socket files removed.
//!
//! M3-a: the daemon can ALSO serve loopback TCP (`--listen
//! tcp:PORT[+unix:PATH]`) for the webview GUI — same router, same
//! semantics. The API is unauthenticated, so TCP is loopback-only by
//! construction (bind 127.0.0.1, never anything wider).

mod cli;

use anyhow::Context;
use clap::Parser;
use peregrine_engine_http::SegmentConfig;
use peregrine_scheduler::SchedulerConfig;
use peregrine_server::{daemon::Daemon, uds};

use crate::cli::{Args, Listen};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let listen = args.listen_spec()?;
    if args.print_socket {
        match &listen {
            Listen::Unix(p) => println!("{}", p.display()),
            Listen::Tcp { port, also_unix } => {
                if let Some(p) = also_unix {
                    println!("unix:{}", p.display());
                }
                println!("tcp:{port}");
            }
        }
        return Ok(());
    }

    // ---- 1. Bind every listener (ownership proof) -----------------
    let mut bound_unix: Vec<(std::path::PathBuf, uds::SocketIdentity)> = Vec::new();
    let mut unix_listeners = Vec::new();
    let mut tcp_listener = None;
    let tcp_port;

    match &listen {
        Listen::Unix(path) => {
            let (l, id) = uds::bind(path).await?;
            bound_unix.push((path.clone(), id));
            unix_listeners.push(l);
            tcp_port = None;
        }
        Listen::Tcp { port, also_unix } => {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], *port));
            tcp_listener = Some(
                tokio::net::TcpListener::bind(addr)
                    .await
                    .with_context(|| format!("bind tcp {addr}"))?,
            );
            if let Some(p) = also_unix {
                let (l, id) = uds::bind(p).await?;
                bound_unix.push((p.clone(), id));
                unix_listeners.push(l);
            }
            tcp_port = Some(*port);
        }
    }

    // ---- 2. Build + start (crash recovery BEFORE serving) ---------
    // db: flag > PGRG_DB env (test/smoke harnesses) > XDG default.
    let db = args
        .db
        .clone()
        .or_else(|| std::env::var_os("PGRG_DB").map(std::path::PathBuf::from));
    let daemon = std::sync::Arc::new(Daemon::build(
        db.as_deref(),
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

    // ---- 3. Serve (same router on every listener) -----------------
    let app = daemon.router();
    tracing::info!(
        tcp = tcp_port.map(|p| p.to_string()).unwrap_or_default(),
        unix = bound_unix
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect::<Vec<_>>()
            .join(","),
        db = %daemon.db_path.display(),
        version = peregrine_api::VERSION,
        "peregrined listening"
    );

    // Unix listeners run alongside; each drains on the shutdown
    // signal. (Multiple `shutdown_signal()` futures are fine —
    // tokio broadcasts signals to every subscriber.)
    let mut unix_tasks = Vec::new();
    for l in unix_listeners {
        let app = app.clone();
        unix_tasks.push(tokio::spawn(async move {
            axum::serve(l, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
        }));
    }

    if let Some(l) = tcp_listener {
        axum::serve(l, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("tcp server run loop")?;
    } else {
        // Unix-only: drive the (single) serve task on the main
        // future so errors propagate directly.
        let (only, rest) = unix_tasks.split_at_mut(1);
        for t in rest {
            t.abort();
        }
        if let Some(t) = only.first_mut() {
            t.await.context("unix server run loop")??;
        }
    }

    // ---- 4. Shutdown: engines drain, THEN socket files go --------
    daemon.sched.shutdown().await;
    for (p, id) in &bound_unix {
        uds::remove_socket_file(p, *id).await;
    }
    tracing::info!("peregrined stopped, sockets cleaned");
    Ok(())
}

/// Exit cleanly on SIGINT (^C, interactive) and SIGTERM (systemd/kill).
/// Both paths funnel into axum's graceful shutdown, which drains in-flight
/// requests before `remove_socket_file` runs.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install ctrl-c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
