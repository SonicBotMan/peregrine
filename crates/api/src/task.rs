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
/// State machine (M2 formalizes transitions):
/// `Queued -> Running -> (Paused <-> Running) -> Completed | Failed`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
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
    /// Unix epoch seconds.
    pub created_at: u64,
}

impl Task {
    /// Convenience constructor for tests and callers that do not care about
    /// the remaining fields yet.
    pub fn new(id: TaskId, url: impl Into<String>, save_path: impl Into<String>) -> Self {
        Self {
            id,
            url: url.into(),
            save_path: save_path.into(),
            status: TaskStatus::Queued,
            total_bytes: None,
            received_bytes: 0,
            created_at: unix_now(),
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
