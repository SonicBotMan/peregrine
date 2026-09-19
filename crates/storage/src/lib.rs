//! Peregrine persistence: the segment table (PROPOSAL §5.2).
//!
//! One SQLite file (WAL) per daemon holds the durable download state:
//! a `tasks` row per (url, sink) target and a `segments` row per planned
//! byte range with its confirmed cursor `done`. The whole point is
//! byte-exact resume after kill -9: a worker only ever advances `done`
//! past bytes it has *written and will not revisit*, so a resumed
//! session re-requests `[start+done, end]` per segment and the file is
//! never glued from mismatched versions (task-level `etag` guards the
//! whole set).
//!
//! Crash-ordering caveat (P1-4, M1-c1 R2): WAL commits and file page
//! writeback are unordered across a *power* loss, so a cursor can
//! theoretically lead the bytes it describes. That window is
//! deliberately accepted — closing it needs an fsync per cursor
//! flush — and the engine's sink-length consistency check turns it
//! into a visible "resume mismatch" error, never silent corruption.
//!
//! Concurrency model: rusqlite is synchronous, so every public method
//! is an `async fn` that parks the (tiny) transaction on the blocking
//! pool via `spawn_blocking`. Cursors are updated with `MAX(done, ?)` —
//! monotone by construction, safe against out-of-order workers.
//!
//! M1-c1 scope: this is engine-private state, not yet the M2 task
//! system; the schema grows a task state machine when task-manager
//! lands (B1/B9 track that migration).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

mod downloads;

/// Durable identifier of a download task row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(pub i64);

/// One planned byte range `[start, end]` (inclusive) and its confirmed
/// cursor. `done` counts bytes *within the segment* that are on disk:
/// the resumable frontier of this segment is `start + done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentState {
    pub idx: u32,
    pub start: u64,
    pub end: u64,
    pub done: u64,
}

impl SegmentState {
    /// Byte offset the next session must fetch from (exclusive frontier).
    pub fn frontier(&self) -> u64 {
        self.start + self.done
    }

    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_complete(&self) -> bool {
        self.frontier() > self.end
    }
}

/// Everything the engine needs to resume a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskState {
    pub id: TaskId,
    pub url: String,
    pub sink: PathBuf,
    pub total: Option<u64>,
    /// Task-level validator (strong ETag, quoted wire form, or
    /// Last-Modified string) as LAST served. A change on resume wipes
    /// the segment set (PROPOSAL §5.2: ETag 变更检测 → 全部重验).
    pub etag: Option<String>,
    pub segments: Vec<SegmentState>,
}

/// A tiny handle around one SQLite connection (WAL mode).
///
/// All methods are async and park their transaction on the blocking
/// pool; clone the `Arc<Store>` freely. Not `Sync`-intimate: the
/// connection hides behind a `Mutex`, transactions are µs-scale.
#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

impl Store {
    /// Open (creating if needed) the database at `path`, enabling WAL
    /// and running the schema migration. Foreign keys ON so task
    /// deletion cascades its segments.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating storage dir {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening storage {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("setting WAL mode")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("setting synchronous=NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("enabling foreign keys")?;
        conn.execute_batch(SCHEMA)
            .context("running schema migration")?;
        downloads::migrate(&conn)?;
        Ok(Store(Arc::new(Mutex::new(conn))))
    }

    /// In-memory store for tests and ephemeral sessions.
    pub fn open_memory() -> Result<Store> {
        let conn = Connection::open_in_memory().context("opening :memory: store")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("enabling foreign keys")?;
        conn.execute_batch(SCHEMA)
            .context("running schema migration")?;
        downloads::migrate(&conn)?;
        Ok(Store(Arc::new(Mutex::new(conn))))
    }

    /// Create or refresh the task row for (url, sink). Returns the id.
    /// Total/validator are updated in place — callers treat a mismatched
    /// `etag` on reload as "wipe and restart" (see `replace_segments`).
    pub async fn upsert_task(
        &self,
        url: &str,
        sink: &Path,
        total: Option<u64>,
        etag: Option<&str>,
    ) -> Result<TaskId> {
        let url = url.to_string();
        let sink = sink.to_string_lossy().into_owned();
        let etag = etag.map(str::to_string);
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute(
                "INSERT INTO tasks (url, sink, total, etag) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(url, sink)
                 DO UPDATE SET total = ?3, etag = COALESCE(?4, etag)",
                params![url, sink, total.map(|t| t as i64), etag],
            )
            .context("upserting task")?;
            Ok(TaskId(conn.last_insert_rowid()))
        })
        .await
        .context("join upsert_task")?
    }

    /// Load the full resumable state for (url, sink), if present.
    pub async fn get_task(&self, url: &str, sink: &Path) -> Result<Option<TaskState>> {
        let url = url.to_string();
        let sink = sink.to_string_lossy().into_owned();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT id, total, etag FROM tasks WHERE url = ?1 AND sink = ?2",
                    params![url, sink],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .context("querying task")?;
            let Some((id, total, etag)) = row else {
                return Ok(None);
            };
            let mut stmt = conn
                .prepare(
                    "SELECT idx, start, end, done FROM segments
                     WHERE task_id = ?1 ORDER BY idx",
                )
                .context("preparing segments query")?;
            let segments = stmt
                .query_map(params![id], |r| {
                    Ok(SegmentState {
                        idx: r.get::<_, i64>(0)? as u32,
                        start: r.get::<_, i64>(1)? as u64,
                        end: r.get::<_, i64>(2)? as u64,
                        done: r.get::<_, i64>(3)? as u64,
                    })
                })
                .context("querying segments")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("reading segment rows")?;
            Ok(Some(TaskState {
                id: TaskId(id),
                url,
                sink: sink.into(),
                total: total.map(|t| t as u64),
                etag,
                segments,
            }))
        })
        .await
        .context("join get_task")?
    }

    /// Atomically replace the segment set — used both for a fresh plan
    /// and for the "validator changed, wipe everything" path. One
    /// transaction: partial plans never become visible.
    pub async fn replace_segments(&self, task: TaskId, ranges: &[(u64, u64)]) -> Result<()> {
        let ranges: Vec<(i64, i64)> = ranges.iter().map(|&(s, e)| (s as i64, e as i64)).collect();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = this.lock().unwrap();
            let tx = conn.transaction().context("opening replace tx")?;
            tx.execute("DELETE FROM segments WHERE task_id = ?1", params![task.0])
                .context("clearing old segments")?;
            {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO segments (task_id, idx, start, end, done)
                         VALUES (?1, ?2, ?3, ?4, 0)",
                    )
                    .context("preparing segment insert")?;
                for (idx, &(start, end)) in ranges.iter().enumerate() {
                    stmt.execute(params![task.0, idx as i64, start, end])
                        .context("inserting segment")?;
                }
            }
            tx.commit().context("committing replace tx")?;
            Ok(())
        })
        .await
        .context("join replace_segments")?
    }

    /// Dynamic rebalancing (roadmap item 1): split segment `idx` at
    /// byte `at` — the segment keeps `[start, at]` and a NEW segment
    /// (next free idx) takes `[at+1, old_end]`. One transaction: the
    /// cover invariant (sum of lens == total) never breaks on disk,
    /// so a crash right after a split resumes into a consistent plan.
    /// Returns the new segment's idx. `at` must be inside the segment
    /// (`start <= at < end`).
    pub async fn split_segment(&self, task: TaskId, idx: u32, at: u64) -> Result<u32> {
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = this.lock().unwrap();
            let tx = conn.transaction().context("opening split tx")?;
            let old_end: i64 = tx
                .query_row(
                    "SELECT end FROM segments WHERE task_id = ?1 AND idx = ?2",
                    params![task.0, idx as i64],
                    |r| r.get(0),
                )
                .context("split target segment not found")?;
            let start: i64 = tx
                .query_row(
                    "SELECT start FROM segments WHERE task_id = ?1 AND idx = ?2",
                    params![task.0, idx as i64],
                    |r| r.get(0),
                )
                .context("split target segment not found")?;
            if at < start as u64 || at >= old_end as u64 {
                anyhow::bail!("split point {at} outside segment {idx} range [{start}, {old_end}]");
            }
            let new_idx: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(idx) + 1, 0) FROM segments WHERE task_id = ?1",
                    params![task.0],
                    |r| r.get(0),
                )
                .context("max idx")?;
            tx.execute(
                "UPDATE segments SET end = ?3 WHERE task_id = ?1 AND idx = ?2",
                params![task.0, idx as i64, at as i64],
            )
            .context("shrinking split target")?;
            tx.execute(
                "INSERT INTO segments (task_id, idx, start, end, done)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![task.0, new_idx, (at + 1) as i64, old_end],
            )
            .context("inserting split tail")?;
            tx.commit().context("committing split tx")?;
            Ok(new_idx as u32)
        })
        .await
        .context("join split_segment")?
    }

    /// Advance a segment's confirmed cursor. Monotone (`MAX`): a stale
    /// worker reporting an older frontier cannot move it backwards.
    pub async fn update_cursor(&self, task: TaskId, idx: u32, done: u64) -> Result<()> {
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute(
                "UPDATE segments SET done = MAX(done, ?3)
                 WHERE task_id = ?1 AND idx = ?2",
                params![task.0, idx as i64, done as i64],
            )
            .context("updating segment cursor")?;
            Ok(())
        })
        .await
        .context("join update_cursor")?
    }

    /// Persist the CURRENT validator after a completed session so the
    /// next resume compares against what was actually served.
    pub async fn set_validator(&self, task: TaskId, etag: Option<&str>) -> Result<()> {
        let etag = etag.map(str::to_string);
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute(
                "UPDATE tasks SET etag = ?2 WHERE id = ?1",
                params![task.0, etag],
            )
            .context("updating task validator")?;
            Ok(())
        })
        .await
        .context("join set_validator")?
    }

    /// B36: insert-or-refresh a validator-ONLY row — the write side
    /// of single-stream resume validation. Unlike [`Self::upsert_task`]
    /// this NEVER touches `total`: a concurrent segmented session's
    /// row (total = Some) keeps its meaning, and a fresh single-stream
    /// row is created with total = NULL so Route 1 (segmented resume)
    /// never claims it.
    pub async fn upsert_validator_only(
        &self,
        url: &str,
        sink: &Path,
        etag: &str,
    ) -> Result<TaskId> {
        let url = url.to_string();
        let sink = sink.to_string_lossy().into_owned();
        let etag = etag.to_string();
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute(
                "INSERT INTO tasks (url, sink, total, etag) VALUES (?1, ?2, NULL, ?3)\n                 ON CONFLICT(url, sink)\n                 DO UPDATE SET etag = ?3",
                params![url, sink, etag],
            )
            .context("upserting validator-only row")?;
            Ok(TaskId(conn.last_insert_rowid()))
        })
        .await
        .context("join upsert_validator_only")?
    }

    /// Drop a task and (via FK cascade) its segments. Called when a
    /// download completes — the file on disk is the truth now.
    pub async fn delete_task(&self, task: TaskId) -> Result<()> {
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.lock().unwrap();
            conn.execute("DELETE FROM tasks WHERE id = ?1", params![task.0])
                .context("deleting task")?;
            Ok(())
        })
        .await
        .context("join delete_task")?
    }

    /// B24 orphan GC, run at daemon startup: delete every task row
    /// (cascading its segments) whose SINK FILE no longer exists on
    /// disk. A row without its file can never be resumed — the only
    /// futures it has are a misleading resume attempt or, after a
    /// redirect-target change, sitting orphaned forever. Rows whose
    /// sink still exists are NEVER touched (a validator-only row is
    /// B36's resume state and must survive reboots).
    ///
    /// Returns the number of rows dropped. Best-effort: rows whose
    /// sink path cannot be stat'ed due to a TRANSIENT error are
    /// LEFT ALONE (only a confirmed `NotFound` counts as gone).
    pub async fn purge_missing_sinks(&self) -> Result<usize> {
        let this = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = this.lock().unwrap();
            let rows: Vec<(i64, String)> = guard
                .prepare("SELECT id, sink FROM tasks")
                .context("listing tasks for GC")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .context("scanning tasks for GC")?
                .collect::<rusqlite::Result<_>>()?;
            // One transaction around the whole sweep: atomic (a
            // mid-sweep failure leaves the DB consistent) and a
            // single fsync for N rows instead of N commits (R2
            // P2-3).
            let tx = guard
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("opening GC transaction")?;
            let mut dropped = 0usize;
            for (id, sink) in rows {
                let gone = std::fs::metadata(&sink)
                    .map(|m| !m.is_file())
                    .unwrap_or_else(|e| e.kind() == std::io::ErrorKind::NotFound);
                if gone {
                    tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])
                        .with_context(|| format!("GC deleting task row for missing sink {sink}"))?;
                    dropped += 1;
                }
            }
            tx.commit().context("committing GC sweep")?;
            Ok(dropped)
        })
        .await
        .context("join purge_missing_sinks")?
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    url   TEXT NOT NULL,
    sink  TEXT NOT NULL,
    total INTEGER,
    etag  TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(url, sink)
);
CREATE TABLE IF NOT EXISTS segments (
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    idx     INTEGER NOT NULL,
    start   INTEGER NOT NULL,
    end     INTEGER NOT NULL,
    done    INTEGER NOT NULL,
    PRIMARY KEY (task_id, idx)
) WITHOUT ROWID;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(idx: u32, start: u64, end: u64, done: u64) -> SegmentState {
        SegmentState {
            idx,
            start,
            end,
            done,
        }
    }

    #[tokio::test]
    async fn split_segment_keeps_the_cover_invariant() {
        // Roadmap item 1: the rebalancer's transaction. After a split
        // the plan must still partition the resource exactly — a
        // crash mid-rebalance must never surface a gapped plan.
        let store = Store::open_memory().unwrap();
        let id = store
            .upsert_task(
                "http://x/f",
                std::path::Path::new("/tmp/f.bin"),
                Some(1000),
                None,
            )
            .await
            .unwrap();
        store
            .replace_segments(id, &[(0, 249), (250, 499), (500, 749), (750, 999)])
            .await
            .unwrap();

        let new_idx = store.split_segment(id, 2, 624).await.unwrap();
        let t = store
            .get_task("http://x/f", std::path::Path::new("/tmp/f.bin"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t.segments.len(), 5, "tail segment appended");
        let s2 = &t.segments[2];
        assert_eq!((s2.start, s2.end), (500, 624), "head keeps [start, at]");
        let tail = &t.segments[new_idx as usize];
        assert_eq!(
            (tail.start, tail.end, tail.done),
            (625, 749, 0),
            "tail starts fresh"
        );
        let sum: u64 = t.segments.iter().map(|s| s.len()).sum();
        assert_eq!(sum, 1000, "cover invariant holds across the split");

        // Split point outside the segment → error, plan untouched.
        assert!(store.split_segment(id, 2, 999).await.is_err());
        let t2 = store
            .get_task("http://x/f", std::path::Path::new("/tmp/f.bin"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(t2.segments.len(), 5, "failed split leaves the plan alone");
    }

    #[tokio::test]
    async fn purge_missing_sinks_drops_only_rows_whose_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_memory().unwrap();
        let sink_a = dir.path().join("a.bin");
        let sink_b = dir.path().join("b.bin");
        std::fs::write(&sink_a, b"partial a").unwrap();
        std::fs::write(&sink_b, b"partial b").unwrap();

        // Row A: segmented (total + segments). Row B: B36
        // validator-only. Row C: sink NEVER existed (transient
        // crash between row-write and file-create).
        let a = store
            .upsert_task("http://x/a", &sink_a, Some(10), Some("\"va\""))
            .await
            .unwrap();
        store.replace_segments(a, &[(0, 9)]).await.unwrap();
        store.update_cursor(a, 0, 4).await.unwrap();
        let _b = store
            .upsert_validator_only("http://x/b", &sink_b, "\"vb\"")
            .await
            .unwrap();
        let _c = store
            .upsert_validator_only("http://x/c", &dir.path().join("c.bin"), "\"vc\"")
            .await
            .unwrap();

        // A's file vanishes (user deleted it); B's survives.
        std::fs::remove_file(&sink_a).unwrap();

        let dropped = store.purge_missing_sinks().await.unwrap();
        assert_eq!(dropped, 2, "rows A and C go, B stays");

        assert!(
            store
                .get_task("http://x/a", &sink_a)
                .await
                .unwrap()
                .is_none()
        );
        // FK cascade took A's segment rows too (get via B is the
        // live check; A's segments are unreachable — assert via
        // reopen-free SQL is overkill here).
        let b = store
            .get_task("http://x/b", &sink_b)
            .await
            .unwrap()
            .expect("validator-only row with live sink survives GC");
        assert_eq!(b.etag.as_deref(), Some("\"vb\""));
        assert!(
            store
                .get_task("http://x/c", &dir.path().join("c.bin"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn task_roundtrip_and_reopen_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/segments.db");
        let id = {
            let store = Store::open(&path).unwrap();
            let id = store
                .upsert_task(
                    "http://x/f.bin",
                    Path::new("/tmp/f.bin"),
                    Some(1000),
                    Some("\"v1\""),
                )
                .await
                .unwrap();
            store
                .replace_segments(id, &[(0, 499), (500, 999)])
                .await
                .unwrap();
            store.update_cursor(id, 0, 100).await.unwrap();
            id
        };
        // Reopen from disk: WAL survived, state intact.
        let store = Store::open(&path).unwrap();
        let state = store
            .get_task("http://x/f.bin", Path::new("/tmp/f.bin"))
            .await
            .unwrap()
            .expect("task persisted");
        assert_eq!(state.id, id);
        assert_eq!(state.total, Some(1000));
        assert_eq!(state.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            state.segments,
            vec![seg(0, 0, 499, 100), seg(1, 500, 999, 0)]
        );
    }

    #[tokio::test]
    async fn cursor_updates_are_monotone() {
        let store = Store::open_memory().unwrap();
        let id = store
            .upsert_task("u", Path::new("s"), Some(10), None)
            .await
            .unwrap();
        store.replace_segments(id, &[(0, 9)]).await.unwrap();
        store.update_cursor(id, 0, 7).await.unwrap();
        // A stale report must not rewind the frontier.
        store.update_cursor(id, 0, 3).await.unwrap();
        let state = store.get_task("u", Path::new("s")).await.unwrap().unwrap();
        assert_eq!(state.segments[0].done, 7);
        assert_eq!(state.segments[0].frontier(), 7);
    }

    #[tokio::test]
    async fn replace_segments_wipes_old_set_atomically() {
        let store = Store::open_memory().unwrap();
        let id = store
            .upsert_task("u", Path::new("s"), Some(100), None)
            .await
            .unwrap();
        store
            .replace_segments(id, &[(0, 49), (50, 99)])
            .await
            .unwrap();
        store.update_cursor(id, 1, 10).await.unwrap();
        // Validator changed → full wipe + new plan.
        store.replace_segments(id, &[(0, 99)]).await.unwrap();
        let state = store.get_task("u", Path::new("s")).await.unwrap().unwrap();
        assert_eq!(state.segments, vec![seg(0, 0, 99, 0)]);
    }

    #[tokio::test]
    async fn delete_task_cascades_segments() {
        let store = Store::open_memory().unwrap();
        let id = store
            .upsert_task("u", Path::new("s"), Some(10), None)
            .await
            .unwrap();
        store.replace_segments(id, &[(0, 9)]).await.unwrap();
        store.delete_task(id).await.unwrap();
        assert!(store.get_task("u", Path::new("s")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_is_idempotent_per_url_sink() {
        let store = Store::open_memory().unwrap();
        let a = store
            .upsert_task("u", Path::new("s"), Some(1), None)
            .await
            .unwrap();
        let b = store
            .upsert_task("u", Path::new("s"), Some(2), Some("\"v2\""))
            .await
            .unwrap();
        assert_eq!(a, b, "same (url, sink) is one row");
        let state = store.get_task("u", Path::new("s")).await.unwrap().unwrap();
        assert_eq!(state.total, Some(2));
        assert_eq!(state.etag.as_deref(), Some("\"v2\""));
    }

    #[tokio::test]
    async fn set_validator_overwrites_and_clears() {
        let store = Store::open_memory().unwrap();
        let id = store
            .upsert_task("u", Path::new("s"), None, Some("\"a\""))
            .await
            .unwrap();
        store.set_validator(id, Some("\"b\"")).await.unwrap();
        let state = store.get_task("u", Path::new("s")).await.unwrap().unwrap();
        assert_eq!(state.etag.as_deref(), Some("\"b\""));
        store.set_validator(id, None).await.unwrap();
        let state = store.get_task("u", Path::new("s")).await.unwrap().unwrap();
        assert_eq!(state.etag, None);
    }

    #[tokio::test]
    async fn segment_state_helpers() {
        let s = seg(0, 100, 199, 50);
        assert_eq!(s.len(), 100);
        assert_eq!(s.frontier(), 150);
        assert!(!s.is_complete());
        assert!(seg(0, 100, 199, 100).is_complete());
        assert!(
            seg(0, 100, 199, 999).is_complete(),
            "done can overshoot only if server over-served; frontier > end marks it done"
        );
    }
}
