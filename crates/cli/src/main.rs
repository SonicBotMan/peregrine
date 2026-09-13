//! `pg` — thin CLI client for the peregrine daemon (talks HTTP over UDS).

mod uds_client;

use clap::{Parser, Subcommand};
use peregrine_api::AddTaskRequest;
use peregrine_api::task::{Priority, Task, TaskStatus};

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
    }
    Ok(())
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
}
