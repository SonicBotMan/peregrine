//! `pg` — thin CLI client for the peregrine daemon (talks HTTP over UDS).

use clap::{CommandFactory, Parser, Subcommand};
use peregrine_api::AddTaskRequest;
use peregrine_api::task::{Priority, Task, TaskStatus};

use peregrine_cli::DaemonClient;
/// Peregrine CLI.
#[derive(Debug, Parser)]
#[command(name = "pg", version, about)]
struct Args {
    /// Daemon endpoint: UDS path, or `tcp:PORT` / `tcp:HOST:PORT`
    /// (bare PORT = loopback). PGRG_SOCKET env honored for both
    /// forms.
    #[arg(long, global = true)]
    socket: Option<String>,

    /// Bearer token for daemons started with `--auth-token`.
    /// Env fallback: PGRG_TOKEN. Ignored by UDS daemons.
    #[arg(long, global = true, env = "PGRG_TOKEN")]
    token: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Check the daemon is alive; prints its identity.
    Ping,

    /// Enqueue a download.
    Add {
        /// URL to download.
        url: String,
        /// Where to save the file.
        #[arg(long, short)]
        out: String,
        /// Queue priority (low | normal | high).
        #[arg(long, short, default_value = "normal")]
        priority: Priority,
    },

    /// List tasks (optionally filtered by status).
    List {
        /// Only this status (queued | running | paused | completed | failed).
        #[arg(long, short)]
        status: Option<TaskStatus>,
    },

    /// Show one task in full.
    Get { id: String },

    /// Pause a task.
    Pause { id: String },

    /// Resume a paused task.
    Resume { id: String },

    /// Remove a task (partial files are kept).
    Remove {
        id: String,
        #[arg(long)]
        purge: bool,
    },

    /// Set a task's download rate limit (BT tasks excepted).
    ///
    /// QA-E2E Bug 2: BT tasks are rejected with a clear error —
    /// engine-bt has no throttle plumbing (librqbit owns the
    /// sockets), so a "successful" set would be a silent no-op.
    /// `pg speed` (daemon-wide) still applies to every protocol.
    Limit {
        id: String,
        /// Bytes/sec. 0 = unlimited. Accepts k/m suffixes (64k, 2m).
        #[arg(long, short)]
        bps: String,
    },

    /// Show (or set) the daemon-wide rate limit.
    Speed {
        /// Set instead of show. 0 = unlimited. k/m suffixes OK.
        #[arg(long, short)]
        set: Option<String>,
    },

    /// Generate shell completion for `pg` (M6-c).
    ///
    /// `pg completions bash > /usr/share/bash-completion/completions/pg`
    /// or `pg completions zsh > "${fpath[1]}/_pg"` — emit to stdout,
    /// the shell file is the caller's business.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },

    /// Hidden packaging helper: emit a roff man page for `pg(1)`
    /// into the given directory. Used by scripts/package.sh; not
    /// for end users.
    #[command(hide = true)]
    GenMan {
        /// Output directory (gets pg.1 written into it).
        dir: std::path::PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No TLS here: `pg` is a thin UDS client (R2' P2-4) — the
    // daemon owns the engine side and its TLS setup.
    let args = Args::parse();

    // Completions/man are pure-local: no socket resolution, no
    // daemon needed — emit and exit before any I/O setup.
    match &args.cmd {
        Cmd::Completions { shell } => {
            let mut out = std::io::stdout().lock();
            let mut cmd = Args::command();
            clap_complete::generate(*shell, &mut cmd, "pg", &mut out);
            return Ok(());
        }
        Cmd::GenMan { dir } => {
            std::fs::create_dir_all(dir)?;
            let man = clap_mangen::Man::new(Args::command().name("pg"));
            let mut file = std::fs::File::create(dir.join("pg.1"))?;
            man.render(&mut file)?;
            return Ok(());
        }
        _ => {}
    }

    // --socket / PGRG_SOCKET `tcp:…` → TCP endpoint (the spec must
    // be checked BEFORE socket_path() — a tcp: value flowing into
    // it would be treated as a literal UDS path and dial garbage).
    let raw_spec = args.socket.clone().or_else(|| {
        std::env::var_os("PGRG_SOCKET")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string_lossy().into_owned())
    });
    let client = match raw_spec.as_deref() {
        Some(s) if s.starts_with("tcp:") => {
            DaemonClient::new(peregrine_api::uds_client::Endpoint::parse_checked(s)?)
                .with_token(args.token.clone())
        }
        _ => {
            // Windows has no UDS control channel: any non-`tcp:` spec
            // is rejected, and the default is the loopback TCP port
            // the GUI sidecar serves (`--tcp 8420`), so `pg` speaks to
            // a running desktop app out of the box.
            #[cfg(not(unix))]
            {
                if args.socket.is_some() {
                    anyhow::bail!(
                        "unix-domain sockets are unavailable on Windows; \
                         use --socket tcp:PORT"
                    );
                }
                let endpoint = peregrine_api::uds_client::Endpoint::Tcp(
                    peregrine_api::transport::default_tcp_authority(),
                );
                DaemonClient::new(endpoint).with_token(args.token.clone())
            }
            #[cfg(unix)]
            {
                let socket = peregrine_api::transport::socket_path(args.socket.as_deref())?;
                DaemonClient::new(socket).with_token(args.token.clone())
            }
        }
    };
    match args.cmd {
        Cmd::Ping => {
            let health = client.ping().await?;
            println!("{}", serde_json::to_string_pretty(&health)?);
        }
        Cmd::Add { url, out, priority } => {
            let task: Task = client
                .request_json(
                    "POST",
                    "/tasks",
                    Some(&AddTaskRequest {
                        url,
                        save_path: out,
                        priority,
                    }),
                )
                .await?;
            println!("queued {}", task.id);
        }
        Cmd::List { status } => {
            let path = match status {
                Some(s) => format!("/tasks?status={s}"),
                None => "/tasks".to_string(),
            };
            let tasks: Vec<Task> = client
                .request_json("GET", &path, None::<&serde_json::Value>)
                .await?;
            print_table(&tasks);
        }
        Cmd::Get { id } => {
            let task: Task = client
                .request_json("GET", &format!("/tasks/{id}"), None::<&serde_json::Value>)
                .await?;
            println!("{}", serde_json::to_string_pretty(&task)?);
        }
        Cmd::Pause { id } => {
            let task: Task = client
                .request_json(
                    "POST",
                    &format!("/tasks/{id}/pause"),
                    None::<&serde_json::Value>,
                )
                .await?;
            println!("paused {}", task.id);
        }
        Cmd::Resume { id } => {
            let task: Task = client
                .request_json(
                    "POST",
                    &format!("/tasks/{id}/resume"),
                    None::<&serde_json::Value>,
                )
                .await?;
            println!("queued {}", task.id);
        }
        Cmd::Remove { id, purge } => {
            // The daemon answers `{"removed": true}` — NOT a Task
            // (smoke-found: typed decode died with `missing field id`).
            // `--purge` opts into data deletion (M5.1 P0-2).
            let path = if purge {
                format!("/tasks/{id}?purge=true")
            } else {
                format!("/tasks/{id}")
            };
            let v: serde_json::Value = client
                .request_json("DELETE", &path, None::<&serde_json::Value>)
                .await?;
            anyhow::ensure!(v["removed"].as_bool().unwrap_or(false), "daemon: {v}");
            println!("removed {id}");
        }
        Cmd::Limit { id, bps } => {
            let bps = parse_bps(&bps)?;
            let task: Task = client
                .request_json(
                    "PUT",
                    &format!("/tasks/{id}/limit"),
                    Some(&serde_json::json!({ "bps": bps })),
                )
                .await?;
            println!("limit {} = {}", task.id, human_bps(task.speed_limit_bps));
        }
        Cmd::Speed { set } => match set {
            None => {
                let v: serde_json::Value = client
                    .request_json("GET", "/settings", None::<&serde_json::Value>)
                    .await?;
                let bps = v["global_limit_bps"].as_u64().unwrap_or(0);
                println!("global limit = {}", human_bps(bps));
            }
            Some(raw) => {
                let bps = parse_bps(&raw)?;
                let v: serde_json::Value = client
                    .request_json(
                        "PUT",
                        "/settings",
                        Some(&serde_json::json!({ "global_limit_bps": bps })),
                    )
                    .await?;
                let now = v["global_limit_bps"].as_u64().unwrap_or(bps);
                println!("global limit = {}", human_bps(now));
            }
        },
        Cmd::Completions { .. } | Cmd::GenMan { .. } => {
            unreachable!("handled before daemon setup")
        }
    }
    Ok(())
}

/// `"64k"` → 65536, `"2m"` → 2097152, `"0"` → 0. Bare numbers are
/// bytes/sec.
fn parse_bps(raw: &str) -> anyhow::Result<u64> {
    let raw = raw.trim().to_ascii_lowercase();
    let (num, mult) = match raw.chars().last() {
        Some('k') => (&raw[..raw.len() - 1], 1024u64),
        Some('m') => (&raw[..raw.len() - 1], 1024 * 1024),
        _ => (&raw[..], 1),
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid rate {raw:?}: expected e.g. 128k / 2m / 0"))?;
    Ok(n.saturating_mul(mult))
}

/// Human-readable rate for the success line.
fn human_bps(bps: u64) -> String {
    if bps == 0 {
        "unlimited".to_string()
    } else if bps >= 1024 * 1024 {
        format!("{:.1} MB/s", bps as f64 / (1024.0 * 1024.0))
    } else if bps >= 1024 {
        format!("{:.1} KB/s", bps as f64 / 1024.0)
    } else {
        format!("{bps} B/s")
    }
}

/// Plain-text table: stable columns, no pager, no colors. The GUI is
/// the product surface; the CLI is for humans debugging a daemon.
fn print_table(tasks: &[Task]) {
    // Terminal width: COLUMNS env (set by most shells for TTYs)
    // with an 80 fallback for pipes/CI. The id is NEVER truncated —
    // it is the operating handle (`pg get/pause/remove <id>`), and
    // a clipped id is unusable (acceptance round: take(14) ate 7 of
    // the 21 id chars). The URL column absorbs the width budget
    // instead, clipped head-first — scheme bytes are noise, the
    // filename tail is what the eye needs.
    let cols: usize = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&c| c >= 40)
        .unwrap_or(80);
    let id_w = tasks
        .iter()
        .map(|t| t.id.as_str().chars().count())
        .chain(std::iter::once(2))
        .max()
        .unwrap();
    let url_w = url_width(cols, id_w);
    println!(
        "{:<id_w$} {:<9} {:>10} {:<6} URL",
        "ID", "STATUS", "RECEIVED", "PRIO"
    );
    for t in tasks {
        println!(
            "{:<id_w$} {:<9} {:>10} {:<6} {}",
            t.id.as_str(),
            t.status,
            bytes_fmt(t.received_bytes),
            t.priority,
            clip_url(&t.url, url_w)
        );
    }
}

/// Fixed columns + 3 gaps (id/status/received/prio) → what's left
/// for URL, floored so a tiny terminal still shows a URL tail.
fn url_width(cols: usize, id_w: usize) -> usize {
    cols.saturating_sub(id_w + 1 + 9 + 1 + 10 + 1 + 6 + 1)
        .max(8)
}

/// Head-first clip: keep the tail (filename, query) — the leading
/// scheme/host bytes carry the least information per column.
fn clip_url(url: &str, w: usize) -> String {
    let n = url.chars().count();
    if n <= w {
        return url.to_string();
    }
    let take = w.saturating_sub(1).max(1);
    let skip = n.saturating_sub(take);
    format!("…{}", url.chars().skip(skip).collect::<String>())
}

fn bytes_fmt(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
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
    fn parses_ping_and_socket() {
        let args = Args::try_parse_from(["pg", "--socket", "/tmp/x.sock", "ping"]).unwrap();
        assert_eq!(args.socket.as_deref(), Some("/tmp/x.sock"));
        assert!(matches!(args.cmd, Cmd::Ping));
    }

    // #31: the id is the operating handle — the layout must budget
    // for its FULL width, and URLs clip head-first instead.
    #[test]
    fn url_width_budgets_full_id_and_floors_url() {
        // 21-char id at 80 cols: 80 - (21+1+9+1+10+1+6+1) = 30.
        assert_eq!(url_width(80, 21), 30);
        // Tiny terminal: URL floor keeps a usable tail.
        assert_eq!(url_width(40, 21), 8);
        // Absurd terminal (a pipe, cols below the floor): still 8.
        assert_eq!(url_width(10, 21), 8);
    }

    #[test]
    fn clip_url_keeps_tail_head_first() {
        // exact fit — untouched
        assert_eq!(clip_url("short", 10), "short");
        // over budget: ellipsis + LAST w-1 chars (tail survives)
        let clipped = clip_url("http://a/b/c/file.bin", 10);
        assert_eq!(clipped.chars().count(), 10);
        assert!(clipped.starts_with('…'));
        assert!(clipped.ends_with("file.bin"));
    }

    #[test]
    fn parses_add_with_priority_out() {
        let args = Args::try_parse_from([
            "pg",
            "add",
            "http://x/f.bin",
            "--out",
            "/tmp/f.bin",
            "-p",
            "high",
        ])
        .unwrap();
        match args.cmd {
            Cmd::Add { url, out, priority } => {
                assert_eq!(url, "http://x/f.bin");
                assert_eq!(out, "/tmp/f.bin");
                assert_eq!(priority, Priority::High);
            }
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn parses_list_status() {
        let args = Args::try_parse_from(["pg", "list", "--status", "paused"]).unwrap();
        match args.cmd {
            Cmd::List { status } => assert_eq!(status, Some(TaskStatus::Paused)),
            other => panic!("wrong cmd: {other:?}"),
        }
    }

    #[test]
    fn bytes_units() {
        assert_eq!(bytes_fmt(0), "0 B");
        assert_eq!(bytes_fmt(999), "999 B");
        assert_eq!(bytes_fmt(4096), "4.0 KB");
        assert_eq!(bytes_fmt(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn parses_bps_suffixes() {
        assert_eq!(parse_bps("0").unwrap(), 0);
        assert_eq!(parse_bps("1000").unwrap(), 1_000);
        assert_eq!(parse_bps("64k").unwrap(), 65_536);
        assert_eq!(parse_bps("2m").unwrap(), 2_097_152);
        assert_eq!(parse_bps(" 128K ").unwrap(), 131_072);
        assert!(parse_bps("fast").is_err());
        assert!(parse_bps("").is_err());
    }

    #[test]
    fn humanizes() {
        assert_eq!(human_bps(0), "unlimited");
        assert_eq!(human_bps(999), "999 B/s");
        assert_eq!(human_bps(4096), "4.0 KB/s");
        assert_eq!(human_bps(2 * 1024 * 1024), "2.0 MB/s");
    }
}
