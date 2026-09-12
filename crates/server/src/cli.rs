use clap::Parser;

/// Peregrine download daemon.
#[derive(Debug, Parser)]
#[command(name = "peregrined", version, about)]
pub struct Args {
    /// Unix socket path. Default: $XDG_RUNTIME_DIR/peregrine/peregrine.sock
    /// (falls back to /tmp/peregrine-$UID.sock).
    #[arg(long)]
    pub socket: Option<String>,
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
    fn parses_socket_flag() {
        let args = Args::try_parse_from(["peregrined", "--socket", "/tmp/x.sock"]).unwrap();
        assert_eq!(args.socket.as_deref(), Some("/tmp/x.sock"));
    }
}
