use clap::Parser;

/// Peregrine download daemon.
#[derive(Debug, Parser)]
#[command(name = "peregrined", version, about)]
pub struct Args {
    /// Listen spec(s), '+'-joined or repeated. `unix:PATH` (default
    /// `$XDG_RUNTIME_DIR/peregrine/peregrine.sock`), `tcp:PORT` —
    /// TCP is always loopback-only (the API is unauthenticated;
    /// remote control is a future feature WITH auth, not a bind
    /// flag). GUI mode: `tcp:PORT+unix:PATH` so the webview and the
    /// CLI both keep working.
    #[arg(long = "listen", value_delimiter = '+')]
    pub listen: Vec<String>,

    /// Unix socket path override (shorthand for
    /// `--listen unix:PATH`).
    #[arg(long)]
    pub socket: Option<String>,

    /// Loopback TCP port override (shorthand for `--listen tcp:PORT`).
    #[arg(long)]
    pub tcp: Option<u16>,

    /// Task database path. Default: $XDG_DATA_HOME/peregrine/tasks.db
    /// (falls back to ~/.local/share/peregrine/tasks.db).
    #[arg(long)]
    pub db: Option<std::path::PathBuf>,

    /// Print the resolved socket path (and TCP port, if any) and
    /// exit — IPC discovery for the GUI/CLI.
    #[arg(long)]
    pub print_socket: bool,
}

/// Resolve the raw flags into ONE listen spec (shorthands win over
/// `--listen` only when `--listen` is absent; mixing is an error).
impl Args {
    pub fn listen_spec(&self) -> anyhow::Result<Listen> {
        let has_shorthand = self.socket.is_some() || self.tcp.is_some();
        if !self.listen.is_empty() && has_shorthand {
            anyhow::bail!("--listen cannot be combined with --socket/--tcp");
        }
        if !self.listen.is_empty() {
            return Listen::parse(&self.listen);
        }
        match (self.tcp, &self.socket) {
            (Some(port), unix) => Ok(Listen::Tcp {
                port,
                also_unix: unix.as_deref().map(std::path::PathBuf::from),
            }),
            (None, Some(p)) => Ok(Listen::Unix(std::path::PathBuf::from(p))),
            (None, None) => Ok(Listen::default()),
        }
    }
}

/// Where to serve: Unix socket (default), loopback TCP (GUI/dev —
/// webview fetch cannot reach a UDS), or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    Unix(std::path::PathBuf),
    Tcp {
        port: u16,
        /// Serve this UDS too (CLI keeps working while the GUI runs).
        also_unix: Option<std::path::PathBuf>,
    },
}

impl Listen {
    /// Parse `unix:PATH`, `tcp:PORT` specs (already split on '+').
    pub fn parse(specs: &[String]) -> anyhow::Result<Self> {
        let mut tcp: Option<u16> = None;
        let mut unix: Option<std::path::PathBuf> = None;
        for s in specs {
            let (kind, rest) = s.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("invalid --listen {s:?}: expected unix:PATH or tcp:PORT")
            })?;
            match kind {
                "unix" => {
                    if unix.is_some() {
                        anyhow::bail!("duplicate --listen unix spec (last-wins would silently drop the first)");
                    }
                    unix = Some(std::path::PathBuf::from(rest));
                }
                "tcp" => {
                    let port: u16 = rest
                        .parse()
                        .map_err(|_| anyhow::anyhow!("invalid --listen tcp port {rest:?}"))?;
                    if port == 0 {
                        // Ephemeral: --print_socket would print the
                        // SPEC ("tcp:0"), not the bound port — the
                        // caller's discovery would be a lie.
                        anyhow::bail!("--listen tcp:0 is invalid: an explicit port is required (ephemeral ports break --print_socket discovery)");
                    }
                    if tcp.is_some() {
                        anyhow::bail!("duplicate --listen tcp spec (last-wins would silently drop the first)");
                    }
                    tcp = Some(port);
                }
                other => anyhow::bail!("invalid --listen kind {other:?}: expected unix or tcp"),
            }
        }
        match (tcp, unix) {
            (Some(port), also_unix) => Ok(Listen::Tcp { port, also_unix }),
            (None, Some(p)) => Ok(Listen::Unix(p)),
            (None, None) => Ok(Listen::default()),
        }
    }
}

impl Default for Listen {
    fn default() -> Self {
        Listen::Unix(
            peregrine_api::transport::socket_path(None)
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/peregrine.sock")),
        )
    }
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
        assert_eq!(
            args.listen_spec().unwrap(),
            Listen::Unix("/tmp/x.sock".into())
        );
    }

    #[test]
    fn tcp_zero_is_rejected() {
        // Ephemeral port would break --print_socket discovery: it
        // prints the SPEC, not the bound port.
        let err = Listen::parse(&["tcp:0".to_string()]).unwrap_err();
        assert!(err.to_string().contains("ephemeral"), "{err}");
    }

    #[test]
    fn duplicate_kind_is_rejected() {
        let err = Listen::parse(&[
            "tcp:8420".to_string(),
            "tcp:8421".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
        let err = Listen::parse(&[
            "unix:/a.sock".to_string(),
            "unix:/b.sock".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn parses_tcp_plus_unix() {
        let args =
            Args::try_parse_from(["peregrined", "--listen", "tcp:8420+unix:/tmp/g.sock"]).unwrap();
        assert_eq!(
            args.listen_spec().unwrap(),
            Listen::Tcp {
                port: 8420,
                also_unix: Some("/tmp/g.sock".into())
            }
        );
    }

    #[test]
    fn rejects_mixed_flags() {
        let args = Args::try_parse_from([
            "peregrined",
            "--listen",
            "tcp:8420",
            "--socket",
            "/tmp/x.sock",
        ])
        .unwrap();
        assert!(args.listen_spec().is_err());
    }

    #[test]
    fn rejects_non_local_tcp_kind() {
        assert!(Listen::parse(&["http:80".into()]).is_err());
        assert!(Listen::parse(&["tcp:notaport".into()]).is_err());
        assert!(Listen::parse(&["bare".into()]).is_err());
    }
}
