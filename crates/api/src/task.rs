//! Core task domain types.
//!
//! Defined here exactly once (v1 lesson: three type systems diverge and rot).
//! Keep this module dependency-free: serde + std only.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Opaque unique task identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle states of a download task.
///
/// State machine (formalized M2-a; PROPOSAL §3/§7):
/// ```text
///             add                 start
///  (new) ─────────▶ Queued ────────────▶ Running ────▶ Completed
///                    ▲  │ pause           │  │            ▲
///                    │  ▼                 │  │ fail       │ complete
///                    │  Paused ──resume───┘  ▼            │
///                    │   (re-queue)        Failed         │
///                    └────────── remove (any non-terminal,
///                               or terminal with its row) ──┘
/// ```
/// Task cancellation is `remove`, not a state: a cancelled task leaves
/// no row to transition (the frozen scope has no Cancelled status, and
/// a tombstone state would only serve UI history we do not have yet).
/// That is the USER-facing cancel (delete). The ENGINE-facing pause
/// (M2-b) is different: the engine returns `ApiError::Cancelled`, the
/// scheduler lands the task in `Paused` WITH its row and progress —
/// resume continues from the partial instead of restarting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
}

/// CLI parsing (clap value_parser), matching the wire names.
impl std::str::FromStr for TaskStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(format!(
                "unknown status {s:?} (queued | running | paused | completed | failed)"
            )),
        }
    }
}

/// Wire/display names (match serde rename_all). Used by CLI table
/// output; `to_string()` mirrors exactly what JSON emits.
impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::Paused => "paused",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        })
    }
}

impl TaskStatus {
    /// Terminal states never transition again (M2-a state machine).
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Completed | TaskStatus::Failed)
    }

    /// The single legal transition table. Everything else is a
    /// state-machine violation the manager must refuse, not repair.
    pub fn can_transition(self, next: TaskStatus) -> bool {
        use TaskStatus::*;
        matches!(
            (self, next),
            (Queued, Running)
                // Pause covers queued tasks too: "don't touch this
                // yet" is one intent regardless of whether a worker
                // ever started (aria2/IDM semantics).
                | (Queued, Paused)
                | (Running, Paused)
                | (Running, Completed)
                | (Running, Failed)
                | (Paused, Queued)
                // Failed tasks may be retried by the user.
                //
                // Crash recovery (running -> queued) deliberately has
                // NO entry here: `boot()` recovers via its own SQL
                // bypassing this table, and keeping the pair user-
                // reachable would let `resume()` silently re-queue a
                // task an engine is still downloading (aria2 refuses
                // unpause on non-paused tasks; so do we).
                | (Failed, Queued)
        )
    }
}

/// Queue priority: higher runs first when slots free up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

/// CLI parsing (clap value_parser): lowercase to match the wire
/// format (serde rename_all). The daemon never parses these — only
/// the CLI does.
impl std::str::FromStr for Priority {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "normal" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            _ => Err(format!("unknown priority {s:?} (low | normal | high)")),
        }
    }
}

/// Wire/display names (match serde rename_all).
impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
        })
    }
}

/// A download task — the unit of user intent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// Source URL (HTTP/FTP/magnet/m3u8 — engine decides).
    pub url: String,
    /// Absolute destination path of the final file.
    pub save_path: String,
    pub status: TaskStatus,
    /// Bytes announced by the server, if known (`None` for chunked/unknown).
    pub total_bytes: Option<u64>,
    pub received_bytes: u64,
    /// Queue ordering weight (M2-a).
    pub priority: Priority,
    /// Last error, kept for `Failed` tasks (user-facing; cleared on retry).
    pub error: Option<String>,
    /// Unix epoch seconds.
    pub created_at: u64,
    /// Unix epoch seconds of the last persisted change.
    pub updated_at: u64,
}

impl Task {
    /// Convenience constructor for tests and callers that do not care about
    /// the remaining fields yet.
    pub fn new(id: TaskId, url: impl Into<String>, save_path: impl Into<String>) -> Self {
        let now = unix_now();
        Self {
            id,
            url: url.into(),
            save_path: save_path.into(),
            status: TaskStatus::Queued,
            total_bytes: None,
            received_bytes: 0,
            priority: Priority::Normal,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Seconds since unix epoch, monotonic enough for M0 purposes.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_display_and_as_str() {
        let id = TaskId::new("t-42");
        assert_eq!(id.to_string(), "t-42");
        assert_eq!(id.as_str(), "t-42");
    }

    #[test]
    fn task_status_serde_roundtrip_snake_case() {
        for status in [
            TaskStatus::Queued,
            TaskStatus::Running,
            TaskStatus::Paused,
            TaskStatus::Completed,
            TaskStatus::Failed,
        ] {
            let json = serde_json::to_string(&status).expect("serialize");
            let back: TaskStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, status);
        }
        assert_eq!(
            serde_json::to_string(&TaskStatus::Queued).unwrap(),
            "\"queued\""
        );
    }

    #[test]
    fn new_task_starts_queued_with_timestamp() {
        let t = Task::new(TaskId::new("a"), "https://example.com/f.iso", "/tmp/f.iso");
        assert_eq!(t.status, TaskStatus::Queued);
        assert_eq!(t.received_bytes, 0);
        assert!(t.total_bytes.is_none());
        assert!(t.created_at > 1_700_000_000);
    }
}
