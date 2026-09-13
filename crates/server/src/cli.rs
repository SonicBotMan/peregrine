use clap::Parser;

/// Peregrine download daemon.
#[derive(Debug, Parser)]
#[command(name = "peregrined", version, about)]
pub struct Args {
    /// Unix socket path. Default: $XDG_RUNTIME_DIR/peregrine/peregrine.sock
    /// (falls back to /tmp/peregrine-$UID/peregrine.sock).
    #[arg(long)]
    pub socket: Option<String>,

    /// Task database path. Default: $XDG_DATA_HOME/peregrine/tasks.db
    /// (falls back to ~/.local/share/peregrine/tasks.db).
    #[arg(long)]
    pub db: Option<std::path::PathBuf>,
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

    #[test]
    fn parses_db_flag() {
        let args = Args::try_parse_from(["peregrined", "--db", "/tmp/x.db"]).unwrap();
        assert_eq!(args.db, Some(std::path::PathBuf::from("/tmp/x.db")));
    }
}
