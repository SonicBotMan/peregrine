//! User-facing download task persistence (M2-a).
//!
//! Distinct from the engine-level `tasks`/`segments` tables (which are
//! the segmented engine's private resume state, keyed by
//! `(url, sink)`): `downloads` rows are the user's queue — one per
//! download intent, keyed by an opaque `api::TaskId` string, living
//! across daemon restarts.

use anyhow::{Context, Result};
use peregrine_api::{Priority, Task, TaskId, TaskStatus};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Store;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS downloads (
    id         TEXT PRIMARY KEY,
    url        TEXT NOT NULL,
    save_path  TEXT NOT NULL,
    status     TEXT NOT NULL,
    total      INTEGER,
    received   INTEGER NOT NULL DEFAULT 0,
    priority   TEXT NOT NULL DEFAULT 'normal',
    error      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    speed_limit_bps INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Ensure the downloads schema exists (idempotent, part of `Store::open`).
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)
        .context("running downloads schema migration")?;
    // Pre-M3 databases lack the limit column; SQLite has no
    // "ADD COLUMN IF NOT EXISTS", so try once and treat a duplicate
    // column error as success (any other error is real).
    match conn.execute(
        "ALTER TABLE downloads ADD COLUMN speed_limit_bps INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, msg))
            if e.extended_code == rusqlite::ffi::SQLITE_ERROR
                && msg
                    .as_deref()
                    .is_some_and(|m| m.contains("duplicate column")) =>
        {
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(e)).context("adding speed_limit_bps column"),
    }
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: TaskId::new(r.get::<_, String>(0)?),
        url: r.get(1)?,
        save_path: r.get(2)?,
        status: parse_status(r.get::<_, String>(3)?),
        total_bytes: r.get::<_, Option<i64>>(4)?.map(|t| t as u64),
        received_bytes: r.get::<_, i64>(5)? as u64,
        priority: parse_priority(r.get::<_, String>(6)?),
        error: r.get(7)?,
        created_at: r.get::<_, i64>(8)? as u64,
        updated_at: r.get::<_, i64>(9)? as u64,
        speed_limit_bps: r.get::<_, i64>(10).unwrap_or(0).max(0) as u64,
    })
}

fn parse_status(s: String) -> TaskStatus {
    match s.as_str() {
        "running" => TaskStatus::Running,
        "paused" => TaskStatus::Paused,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "queued" => TaskStatus::Queued,
        other => {
            // Degrade, but say so: a hand-corrupted row re-downloading
            // silently is the exact failure this warn exists to surface.
            tracing::warn!(status = other, "corrupt status column, degrading to queued");
            TaskStatus::Queued
        }
    }
}

fn parse_priority(s: String) -> Priority {
    match s.as_str() {
        "high" => Priority::High,
        "low" => Priority::Low,
        _ => Priority::Normal,
    }
}

fn status_str(s: TaskStatus) -> String {
    match s {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Paused => "paused",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
    }
    .into()
}

fn priority_str(p: Priority) -> String {
    match p {
        Priority::High => "high",
        Priority::Normal => "normal",
        Priority::Low => "low",
    }
    .into()
}

impl Store {
    /// Persist a new task row. The id must be fresh (caller-generated);
    /// a collision is an integrity error, not an upsert.
    pub async fn insert_download(&self, task: &Task) -> Result<()> {
        let task = task.clone();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute(
                "INSERT INTO downloads
                   (id, url, save_path, status, total, received,
                    priority, error, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    task.id.as_str(),
                    task.url,
                    task.save_path,
                    status_str(task.status),
                    task.total_bytes.map(|t| t as i64),
                    task.received_bytes as i64,
                    priority_str(task.priority),
                    task.error,
                    task.created_at as i64,
                    task.updated_at as i64,
                ],
            )
            .context("inserting download")?;
            Ok(())
        })
        .await
        .context("join insert_download")?
    }

    /// Load one task by id.
    pub async fn get_download(&self, id: &TaskId) -> Result<Option<Task>> {
        let id = id.as_str().to_string();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.query_row(
                "SELECT id, url, save_path, status, total, received,
                        priority, error, created_at, updated_at, speed_limit_bps
                 FROM downloads WHERE id = ?1",
                params![id],
                row_to_task,
            )
            .optional()
            .context("loading download")
        })
        .await
        .context("join get_download")?
    }

    /// All tasks, or one status's tasks. Queue order: higher priority
    /// first, then oldest created — exactly the order the scheduler
    /// should hand out slots in.
    pub async fn list_downloads(&self, status: Option<TaskStatus>) -> Result<Vec<Task>> {
        let status = status.map(status_str);
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT id, url, save_path, status, total, received,
                            priority, error, created_at, updated_at, speed_limit_bps
                     FROM downloads
                     WHERE (?1 IS NULL OR status = ?1)
                     ORDER BY CASE priority
                                WHEN 'high' THEN 0
                                WHEN 'normal' THEN 1
                                ELSE 2
                              END,
                              created_at ASC,
                              id ASC",
                )
                .context("preparing list_downloads")?;
            let rows = stmt
                .query_map(params![status], row_to_task)
                .context("listing downloads")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("reading download rows")?;
            Ok(rows)
        })
        .await
        .context("join list_downloads")?
    }

    /// The id of the ACTIVE (non-terminal) task already bound to
    /// this (url, save_path) pair, if any — the duplicate guard for
    /// `add()`. Double-adding the same link is a download manager's
    /// most common user action, and letting it through would point
    /// two engines at one file (silent corruption), or worse, map
    /// both sessions onto one engine task row via its
    /// `UNIQUE(url, sink)`.
    pub async fn find_active_download_by_target(
        &self,
        url: &str,
        save_path: &str,
    ) -> Result<Option<TaskId>> {
        let url = url.to_string();
        let save_path = save_path.to_string();
        let this = self.0.clone();
        let hit = tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.query_row(
                "SELECT id FROM downloads
                 WHERE url = ?1 AND save_path = ?2
                   AND status NOT IN ('completed', 'failed')
                 LIMIT 1",
                params![url, save_path],
                |row| row.get::<_, String>("id"),
            )
            .optional()
            .context("finding active download by target")
        })
        .await
        .context("join find_active_download_by_target")??;
        Ok(hit.map(TaskId::new))
    }

    /// Compare-and-set status transition: lands only if the row
    /// still shows `from`. Returns `false` when zero rows matched —
    /// either the id is gone or the status moved underneath us; the
    /// caller re-reads to tell those apart. This is the concurrency
    /// guard the state machine (api::TaskStatus) cannot provide by
    /// itself: a check-then-write pair interleaves at every await,
    /// even under SQLite's single-writer discipline.
    ///
    /// `error` is written verbatim — the "retry clears the reason"
    /// rule belongs to the manager, which passes `None` for it.
    pub async fn update_download_status(
        &self,
        id: &TaskId,
        from: TaskStatus,
        to: TaskStatus,
        error: Option<&str>,
        updated_at: u64,
    ) -> Result<bool> {
        let id = id.as_str().to_string();
        let error = error.map(str::to_string);
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            let n = conn
                .execute(
                    "UPDATE downloads
                     SET status = ?3,
                         error = ?4,
                         updated_at = ?5
                     WHERE id = ?1 AND status = ?2",
                    params![
                        id,
                        status_str(from),
                        status_str(to),
                        error,
                        updated_at as i64
                    ],
                )
                .context("updating download status")?;
            Ok(n > 0)
        })
        .await
        .context("join update_download_status")?
    }

    /// Progress tick: received bytes (and, when newly learned, the
    /// total). High-frequency by design — one UPDATE per coalesced
    /// tick, no `updated_at` stamping (progress is not a lifecycle
    /// change; re-sorting the queue on every byte would be churn).
    ///
    /// `received` is clamped monotonic (`MAX`) so a late engine tick
    /// cannot rewind progress, and the write only lands while the
    /// task is `running` — ticks racing a completion are dropped as
    /// the noise they are. Returns `false` when nothing matched.
    pub async fn update_download_progress(
        &self,
        id: &TaskId,
        received: u64,
        total: Option<u64>,
    ) -> Result<bool> {
        let id = id.as_str().to_string();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            let n = conn
                .execute(
                    "UPDATE downloads
                     SET received = MAX(received, ?2),
                         total = CASE WHEN ?3 IS NOT NULL THEN ?3 ELSE total END
                     WHERE id = ?1 AND status NOT IN ('completed', 'failed')",
                    params![id, received as i64, total.map(|t| t as i64)],
                )
                .context("updating download progress")?;
            Ok(n > 0)
        })
        .await
        .context("join update_download_progress")?
    }

    /// Persist a task's rate limit (0 = unlimited). Touches
    /// `updated_at` so GUI ordering stays sane on limit changes.
    pub async fn update_download_limit(&self, id: &TaskId, bps: u64) -> Result<Option<Task>> {
        let id = id.as_str().to_string();
        let this = self.0.clone();
        let task = tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            let n = conn
                .execute(
                    "UPDATE downloads SET speed_limit_bps = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, bps as i64, peregrine_api::task::unix_now()],
                )
                .context("updating download limit")?;
            if n == 0 {
                return Ok(None);
            }
            conn.query_row(
                "SELECT id, url, save_path, status, total, received, priority, error, created_at, updated_at, speed_limit_bps FROM downloads WHERE id = ?1",
                params![id],
                row_to_task,
            )
            .map(Some)
            .context("re-reading task after limit update")
        })
        .await
        .context("join update_download_limit")??;
        Ok(task)
    }

    /// Read a settings key (returns None when unset).
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .context("reading setting")
        })
        .await
        .context("join get_setting")?
    }

    /// Write a settings key (upsert).
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .context("writing setting")?;
            Ok(())
        })
        .await
        .context("join set_setting")?
    }

    /// Remove a task row entirely (any status).
    pub async fn delete_download(&self, id: &TaskId) -> Result<()> {
        let id = id.as_str().to_string();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute("DELETE FROM downloads WHERE id = ?1", params![id])
                .context("deleting download")?;
            Ok(())
        })
        .await
        .context("join delete_download")?
    }

    /// Crash recovery (M2-a): every persisted `running` row is a lie
    /// the dead daemon told — re-queue it. Returns how many were
    /// repaired (for the boot log / a recovery event).
    pub async fn recover_running_to_queued(&self) -> Result<usize> {
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            let n = conn
                .execute(
                    "UPDATE downloads
                     SET status = 'queued', updated_at = unixepoch()
                     WHERE status = 'running'",
                    [],
                )
                .context("recovering running downloads")?;
            Ok(n)
        })
        .await
        .context("join recover_running_to_queued")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, url: &str) -> Task {
        Task::new(TaskId::new(id), url, "/tmp/x.bin")
    }

    #[tokio::test]
    async fn insert_get_roundtrip_all_fields() {
        let store = Store::open_memory().unwrap();
        let mut t = task("t1", "http://x/f.iso");
        t.total_bytes = Some(1024);
        t.received_bytes = 512;
        t.priority = Priority::High;
        t.error = Some("boom".into());
        t.updated_at = 42;
        store.insert_download(&t).await.unwrap();

        let back = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(back, t);
    }

    #[tokio::test]
    async fn duplicate_id_is_rejected() {
        let store = Store::open_memory().unwrap();
        store
            .insert_download(&task("t1", "http://x/a"))
            .await
            .unwrap();
        assert!(
            store
                .insert_download(&task("t1", "http://x/b"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn list_orders_priority_then_age() {
        let store = Store::open_memory().unwrap();
        let mut low_old = task("low-old", "http://x/1");
        low_old.priority = Priority::Low;
        low_old.created_at = 1;
        let mut hi_new = task("hi-new", "http://x/2");
        hi_new.priority = Priority::High;
        hi_new.created_at = 99;
        let norm_mid = task("norm-mid", "http://x/3");
        let norm_mid = Task {
            created_at: 50,
            ..norm_mid
        };
        let mut norm_old = task("norm-old", "http://x/4");
        norm_old.created_at = 10;
        let mut paused = task("paused", "http://x/5");
        paused.status = TaskStatus::Paused;

        for t in [&low_old, &hi_new, &norm_mid, &norm_old, &paused] {
            store.insert_download(t).await.unwrap();
        }

        let ids: Vec<String> = store
            .list_downloads(None)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id.as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            vec!["hi-new", "norm-old", "norm-mid", "paused", "low-old"]
        );

        let queued: Vec<String> = store
            .list_downloads(Some(TaskStatus::Queued))
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id.as_str().to_string())
            .collect();
        assert_eq!(queued, vec!["hi-new", "norm-old", "norm-mid", "low-old"]);
    }

    #[tokio::test]
    async fn status_update_is_cas_guarded() {
        let store = Store::open_memory().unwrap();
        store
            .insert_download(&task("t1", "http://x/a"))
            .await
            .unwrap();
        store
            .update_download_status(
                &TaskId::new("t1"),
                TaskStatus::Queued,
                TaskStatus::Running,
                None,
                5,
            )
            .await
            .unwrap();
        store
            .update_download_status(
                &TaskId::new("t1"),
                TaskStatus::Running,
                TaskStatus::Failed,
                Some("conn reset"),
                9,
            )
            .await
            .unwrap();
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.status, TaskStatus::Failed);
        assert_eq!(t.error.as_deref(), Some("conn reset"));

        // User retries: requeue wipes the stale error (manager passes None).
        store
            .update_download_status(
                &TaskId::new("t1"),
                TaskStatus::Failed,
                TaskStatus::Queued,
                None,
                11,
            )
            .await
            .unwrap();
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.status, TaskStatus::Queued);
        assert_eq!(t.error, None);

        // Stale writer (thought it was Running, row says Queued): refused.
        let won = store
            .update_download_status(
                &TaskId::new("t1"),
                TaskStatus::Running,
                TaskStatus::Paused,
                None,
                12,
            )
            .await
            .unwrap();
        assert!(
            !won,
            "CAS must refuse when the expected predecessor is gone"
        );
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.status, TaskStatus::Queued, "refused write must not land");

        // Unknown id: also just false, the caller classifies.
        assert!(
            !store
                .update_download_status(
                    &TaskId::new("ghost"),
                    TaskStatus::Queued,
                    TaskStatus::Running,
                    None,
                    5
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn progress_update_keeps_known_total_and_clamps() {
        let store = Store::open_memory().unwrap();
        store
            .insert_download(&task("t1", "http://x/a"))
            .await
            .unwrap();
        store
            .update_download_status(
                &TaskId::new("t1"),
                TaskStatus::Queued,
                TaskStatus::Running,
                None,
                5,
            )
            .await
            .unwrap();
        store
            .update_download_progress(&TaskId::new("t1"), 100, Some(1000))
            .await
            .unwrap();
        // Chunked follow-up: total None must not clobber the learned 1000.
        store
            .update_download_progress(&TaskId::new("t1"), 200, None)
            .await
            .unwrap();
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.received_bytes, 200);
        assert_eq!(t.total_bytes, Some(1000));

        // A late out-of-order tick cannot rewind progress.
        store
            .update_download_progress(&TaskId::new("t1"), 50, None)
            .await
            .unwrap();
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.received_bytes, 200, "progress is monotonic");

        // Ticks after completion are dropped, not errors.
        store
            .update_download_status(
                &TaskId::new("t1"),
                TaskStatus::Running,
                TaskStatus::Completed,
                None,
                9,
            )
            .await
            .unwrap();
        let landed = store
            .update_download_progress(&TaskId::new("t1"), 900, None)
            .await
            .unwrap();
        assert!(!landed, "ticks racing completion are noise");
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.received_bytes, 200);
    }

    #[tokio::test]
    async fn recovery_requeues_running_only() {
        let store = Store::open_memory().unwrap();
        let mut run = task("run", "http://x/1");
        run.status = TaskStatus::Running;
        run.received_bytes = 512;
        run.total_bytes = Some(1024);
        let mut done = task("done", "http://x/2");
        done.status = TaskStatus::Completed;
        store.insert_download(&run).await.unwrap();
        store.insert_download(&done).await.unwrap();

        let repaired = store.recover_running_to_queued().await.unwrap();
        let queued = store
            .list_downloads(Some(TaskStatus::Queued))
            .await
            .unwrap();
        assert_eq!(repaired, 1);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id.as_str(), "run");
        // Progress survives recovery — the engine's resume state
        // (segments) picks up from here.
        assert_eq!(queued[0].received_bytes, 512);
        assert_eq!(queued[0].total_bytes, Some(1024));
        let done = store
            .get_download(&TaskId::new("done"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.db");
        {
            let store = Store::open(&path).unwrap();
            store
                .insert_download(&task("t1", "http://x/a"))
                .await
                .unwrap();
            store
                .update_download_status(
                    &TaskId::new("t1"),
                    TaskStatus::Queued,
                    TaskStatus::Running,
                    None,
                    5,
                )
                .await
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let repaired = store.recover_running_to_queued().await.unwrap();
        assert_eq!(repaired, 1);
        let queued = store
            .list_downloads(Some(TaskStatus::Queued))
            .await
            .unwrap();
        assert_eq!(queued[0].id.as_str(), "t1");
        assert_eq!(queued[0].status, TaskStatus::Queued);
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let store = Store::open_memory().unwrap();
        store
            .insert_download(&task("t1", "http://x/a"))
            .await
            .unwrap();
        store.delete_download(&TaskId::new("t1")).await.unwrap();
        assert!(
            store
                .get_download(&TaskId::new("t1"))
                .await
                .unwrap()
                .is_none()
        );
        // Deleting again is a no-op, not an error.
        store.delete_download(&TaskId::new("t1")).await.unwrap();
    }

    #[tokio::test]
    async fn same_second_creations_have_deterministic_order() {
        let store = Store::open_memory().unwrap();
        for name in ["c", "a", "b"] {
            let t = Task {
                created_at: 1000, // identical second
                ..task(name, "http://x/1")
            };
            store.insert_download(&t).await.unwrap();
        }
        let ids: Vec<String> = store
            .list_downloads(None)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id.as_str().to_string())
            .collect();
        // id is lexicographic-chronological (nanosecond ids), so it
        // breaks the tie the scheduler's head-pick needs.
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn corrupt_status_column_degrades_to_queued() {
        let store = Store::open_memory().unwrap();
        store
            .insert_download(&task("t1", "http://x/a"))
            .await
            .unwrap();
        // Hand-corrupted row (the parse guard's whole reason to exist).
        {
            let this = store.0.clone();
            let conn = this.lock().unwrap();
            conn.execute("UPDATE downloads SET status = 'nonsense'", [])
                .unwrap();
        }
        let t = store
            .get_download(&TaskId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.status, TaskStatus::Queued);
    }
}
