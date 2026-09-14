//! `pg` — thin CLI client for the peregrine daemon (talks HTTP over UDS).

use clap::{Parser, Subcommand};
use peregrine_api::AddTaskRequest;
use peregrine_api::task::{Priority, Task, TaskStatus};

use peregrine_cli::DaemonClient;
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
    Remove { id: String },

    /// Set a task's download rate limit.
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No TLS here: `pg` is a thin UDS client (R2' P2-4) — the
    // daemon owns the engine side and its TLS setup.
    let args = Args::parse();
    let socket = peregrine_api::transport::socket_path(args.socket.as_deref())?;
    let client = DaemonClient::new(socket);

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
        Cmd::Remove { id } => {
            // The daemon answers `{"removed": true}` — NOT a Task
            // (smoke-found: typed decode died with `missing field id`).
            let v: serde_json::Value = client
                .request_json(
                    "DELETE",
                    &format!("/tasks/{id}"),
                    None::<&serde_json::Value>,
                )
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
    println!(
        "{:<14} {:<9} {:>10} {:<7} URL",
        "ID", "STATUS", "RECEIVED", "PRIO"
    );
    for t in tasks {
        println!(
            "{:<14} {:<9} {:>10} {:<7} {}",
            t.id.as_str().chars().take(14).collect::<String>(),
            t.status,
            bytes_fmt(t.received_bytes),
            t.priority,
            t.url
        );
    }
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
