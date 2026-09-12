//! `pg` — thin CLI client for the peregrine daemon (talks HTTP over UDS).

mod uds_client;

use clap::{Parser, Subcommand};

use crate::uds_client::DaemonClient;

/// Peregrine CLI.
#[derive(Debug, Parser)]
#[command(name = "pg", version, about)]
struct Args {
    /// Daemon socket path (default: same resolution as peregrined).
    #[arg(long, global = true)]
    socket: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}
#[derive(Debug, Subcommand)]
enum Cmd {
    /// Check the daemon is alive; prints its identity.
    Ping,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let socket = peregrine_api::transport::socket_path(args.socket.as_deref())?;
    let client = DaemonClient::new(socket);

    match args.cmd {
        Cmd::Ping => {
            let health = client.ping().await?;
            println!("{}", serde_json::to_string_pretty(&health)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn parses_ping_and_socket() {
        let args = Args::try_parse_from(["pg", "--socket", "/tmp/x.sock", "ping"]).unwrap();
        assert_eq!(args.socket.as_deref(), Some("/tmp/x.sock"));
        assert!(matches!(args.cmd, Cmd::Ping));
    }
}
