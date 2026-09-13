//! Task lifecycle management (M2-a): the state machine guard plus the
//! durable queue, one level above `storage` and one below the
//! scheduler (M2-b).
//!
//! Responsibilities, deliberately narrow:
//! - own every `TaskStatus` transition (legality checked HERE, refused
//!   loudly — never "repaired" silently: a surprise transition is a
//!   bug somewhere else, and hiding it corrupts the audit trail),
//! - persist each lifecycle change (SQLite is the queue; there is no
//!   in-memory copy to drift),
//! - publish the matching `EngineEvent`,
//! - crash recovery on boot (`running` rows are lies a dead daemon
//!   told — re-queued, engine resume state does the rest).
//!
//! NOT here: starting engines, concurrency, retries-with-backoff —
//! that is the scheduler's contract (M2-b). The manager is the
//! book-keeper; the scheduler is the muscle.

use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use peregrine_api::{EngineEvent, EventBus, Priority, Task, TaskId, TaskStatus, unix_now};
use peregrine_storage::Store;

/// Everything a lifecycle call can fail with. RPC-facing callers
/// (M2-c) map these to transport errors; the distinction they need —
/// "user asked for something impossible" vs "storage broke" — survives.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TaskError {
    #[error("task {0} not found")]
    NotFound(TaskId),
    #[error("illegal transition {from:?} -> {to:?} for task {id}")]
    IllegalTransition {
        id: TaskId,
        from: TaskStatus,
        to: TaskStatus,
    },
    #[error("url must be a non-empty string")]
    EmptyUrl,
    #[error("save_path must be a non-empty absolute path, got {0:?}")]
    InvalidSavePath(String),
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

/// Fresh task ids: nanosecond timestamp × process-unique counter.
/// Locally unique with overwhelming margin (single writer, ~µs floor
/// between ids), sortable by creation time, and no dependency on a
/// uuid crate for a value only THIS daemon ever mints.
fn fresh_task_id() -> TaskId {
    use std::sync::atomic::AtomicU64 as A;
    static COUNTER: A = A::new(0);
    static LAST: A = A::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    // Monotonic even if the clock jumps backwards.
    let mut prev = LAST.load(Ordering::Relaxed);
    loop {
        let candidate = now.max(prev.wrapping_add(1));
        match LAST.compare_exchange_weak(prev, candidate, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => {
                let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
                return TaskId::new(format!("{candidate:016x}-{seq:04x}"));
            }
            Err(actual) => prev = actual,
        }
    }
}

pub struct TaskManager {
    store: Store,
    bus: EventBus,
}

impl TaskManager {
    pub fn new(store: Store, bus: EventBus) -> Self {
        Self { store, bus }
    }

    /// Boot-time crash recovery: re-queue persisted `running` rows.
    /// Returns how many were repaired (0 on a clean start).
    ///
    /// No `EngineEvent` is published for the repair: boot runs before
    /// any client subscribes, and events are incremental
    /// notifications, not a state source — a reconnecting client
    /// lists from the store, which is the truth.
    pub async fn boot(&self) -> Result<usize, TaskError> {
        let repaired = self.store.recover_running_to_queued().await?;
        if repaired > 0 {
            tracing::info!(repaired, "crash recovery: re-queued interrupted tasks");
        }
        Ok(repaired)
    }

    /// Enqueue a new download. The task starts `Queued` — the
    /// scheduler decides when it runs.
    pub async fn add(
        &self,
        url: impl Into<String>,
        save_path: impl Into<String>,
        priority: Priority,
    ) -> Result<Task, TaskError> {
        let url = url.into();
        if url.trim().is_empty() {
            return Err(TaskError::EmptyUrl);
        }
        let save_path = save_path.into();
        if !std::path::Path::new(&save_path).is_absolute() {
            return Err(TaskError::InvalidSavePath(save_path));
        }
        let now = unix_now();
        let task = Task {
            id: fresh_task_id(),
            url,
            save_path,
            status: TaskStatus::Queued,
            total_bytes: None,
            received_bytes: 0,
            priority,
            error: None,
            created_at: now,
            updated_at: now,
        };
        self.store.insert_download(&task).await?;
        self.bus.publish(EngineEvent::TaskAdded {
            id: task.id.clone(),
            status: TaskStatus::Queued,
        });
        Ok(task)
    }

    pub async fn get(&self, id: &TaskId) -> Result<Option<Task>, TaskError> {
        Ok(self.store.get_download(id).await?)
    }

    /// Queue order: priority desc, then oldest first — the same order
    /// the scheduler should consume in.
    pub async fn list(&self, status: Option<TaskStatus>) -> Result<Vec<Task>, TaskError> {
        Ok(self.store.list_downloads(status).await?)
    }

    /// Scheduler intake (M2-b): claim the head of the queue.
    /// Re-reads the row so the legality check sees persisted truth.
    pub async fn mark_running(&self, id: &TaskId) -> Result<Task, TaskError> {
        self.transition(
            id,
            TaskStatus::Running,
            None,
            EngineEvent::TaskStarted { id: id.clone() },
        )
        .await
    }

    /// Engine success (M2-b).
    pub async fn complete(&self, id: &TaskId) -> Result<Task, TaskError> {
        self.transition(
            id,
            TaskStatus::Completed,
            None,
            EngineEvent::TaskCompleted { id: id.clone() },
        )
        .await
    }

    /// Engine failure (M2-b). The reason is kept on the row for the
    /// UI and cleared on the next retry.
    pub async fn fail(&self, id: &TaskId, reason: impl Into<String>) -> Result<Task, TaskError> {
        let reason = reason.into();
        self.transition(
            id,
            TaskStatus::Failed,
            Some(reason.as_str()),
            EngineEvent::TaskFailed {
                id: id.clone(),
                reason: reason.clone(),
            },
        )
        .await
    }

    /// User pause. Legal from `Queued` and `Running`; stopping the
    /// actual worker is the scheduler's half of the contract.
    pub async fn pause(&self, id: &TaskId) -> Result<Task, TaskError> {
        self.transition(
            id,
            TaskStatus::Paused,
            None,
            EngineEvent::TaskStatusChanged {
                id: id.clone(),
                status: TaskStatus::Paused,
            },
        )
        .await
    }

    /// User resume/retry: `Paused -> Queued` and `Failed -> Queued`
    /// in one call (a retry IS a resume of intent).
    pub async fn resume(&self, id: &TaskId) -> Result<Task, TaskError> {
        self.transition(
            id,
            TaskStatus::Queued,
            None,
            EngineEvent::TaskStatusChanged {
                id: id.clone(),
                status: TaskStatus::Queued,
            },
        )
        .await
    }

    /// Remove the task row (any status). Worker stop + partial-file
    /// cleanup policy live in the scheduler (M2-b); the manager only
    /// guarantees the row is gone and clients hear about it — with
    /// the row snapshot in the event, since afterwards it is the only
    /// copy of url/save_path M2-b will ever see.
    pub async fn remove(&self, id: &TaskId) -> Result<(), TaskError> {
        let current = self
            .store
            .get_download(id)
            .await?
            .ok_or_else(|| TaskError::NotFound(id.clone()))?;
        self.store.delete_download(id).await?;
        self.bus.publish(EngineEvent::TaskRemoved {
            id: id.clone(),
            url: current.url,
            save_path: current.save_path,
        });
        Ok(())
    }

    /// Progress persistence for the scheduler's coalesced ticks
    /// (no event: progress events come from the engine's sink, this
    /// only makes them durable). Ticks for missing or TERMINAL tasks
    /// are dropped silently: they race completion/removal by design,
    /// and that noise is not an error. Non-terminal states (queued /
    /// running / PAUSED) all accept writes — a paused task's final
    /// partial reading must land, or resume loses the UI truth.
    pub async fn update_progress(
        &self,
        id: &TaskId,
        received: u64,
        total: Option<u64>,
    ) -> Result<(), TaskError> {
        // bool outcome deliberately ignored — see doc above.
        let _ = self
            .store
            .update_download_progress(id, received, total)
            .await?;
        Ok(())
    }

    /// The one transition path, compare-and-set: validate against a
    /// row read, then write guarded on that exact predecessor.
    /// A check-then-write pair without the guard interleaves at every
    /// await point (two futures can both validate `Running` and both
    /// write — terminal states overwritten, queue heads
    /// double-claimed); the CAS write is what actually enforces the
    /// state machine under concurrency. `event` fires only on the
    /// winning write.
    async fn transition(
        &self,
        id: &TaskId,
        to: TaskStatus,
        error: Option<&str>,
        event: EngineEvent,
    ) -> Result<Task, TaskError> {
        let current = self
            .store
            .get_download(id)
            .await?
            .ok_or_else(|| TaskError::NotFound(id.clone()))?;
        // Fast path: refuse the clearly-illegal without a write, with
        // the friendliest from-state in the error.
        if !current.status.can_transition(to) {
            return Err(TaskError::IllegalTransition {
                id: id.clone(),
                from: current.status,
                to,
            });
        }
        let won = self
            .store
            .update_download_status(id, current.status, to, error, unix_now())
            .await?;
        if !won {
            // Lost a race: the row moved between our read and write.
            // Re-read so the error reports the world as it is.
            let winner = self
                .store
                .get_download(id)
                .await?
                .ok_or_else(|| TaskError::NotFound(id.clone()))?;
            return Err(TaskError::IllegalTransition {
                id: id.clone(),
                from: winner.status,
                to,
            });
        }
        self.bus.publish(event);
        Ok(Task {
            status: to,
            error: error.map(str::to_string),
            updated_at: unix_now(),
            ..current
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peregrine_api::ProgressSink;
    use std::sync::Arc;

    fn mgr() -> TaskManager {
        TaskManager::new(Store::open_memory().unwrap(), EventBus::new(64))
    }

    /// Drain a subscriber created BEFORE the calls under test
    /// (broadcast: late subscribers see nothing).
    fn drain(rx: &mut tokio::sync::broadcast::Receiver<EngineEvent>) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[tokio::test]
    async fn add_persists_queued_and_emits() {
        let m = mgr();
        let mut rx = m.bus.subscribe();
        let t = m
            .add("http://x/f.iso", "/tmp/f.iso", Priority::High)
            .await
            .unwrap();
        assert_eq!(t.status, TaskStatus::Queued);
        assert_eq!(t.priority, Priority::High);

        let back = m.get(&t.id).await.unwrap().unwrap();
        assert_eq!(back.url, "http://x/f.iso");
        assert_eq!(back.priority, Priority::High);

        match &drain(&mut rx)[0] {
            EngineEvent::TaskAdded { status, .. } => assert_eq!(*status, TaskStatus::Queued),
            other => panic!("expected TaskAdded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_url_refused() {
        let m = mgr();
        assert!(matches!(
            m.add("   ", "/tmp/x", Priority::Normal).await,
            Err(TaskError::EmptyUrl)
        ));
    }

    #[tokio::test]
    async fn relative_save_path_refused() {
        let m = mgr();
        assert!(matches!(
            m.add("http://x/a", "downloads/a.bin", Priority::Normal)
                .await,
            Err(TaskError::InvalidSavePath(_))
        ));
        assert!(matches!(
            m.add("http://x/a", "", Priority::Normal).await,
            Err(TaskError::InvalidSavePath(_))
        ));
    }

    #[tokio::test]
    async fn full_happy_lifecycle() {
        let m = mgr();
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        let t = m.mark_running(&t.id).await.unwrap();
        assert_eq!(t.status, TaskStatus::Running);
        m.update_progress(&t.id, 500, Some(1000)).await.unwrap();
        let t = m.complete(&t.id).await.unwrap();
        assert_eq!(t.status, TaskStatus::Completed);

        let stored = m.get(&t.id).await.unwrap().unwrap();
        assert_eq!(stored.received_bytes, 500);
        assert_eq!(stored.total_bytes, Some(1000));
        // Terminal: nothing legal remains.
        assert!(
            !stored.status.can_transition(TaskStatus::Paused),
            "completed tasks cannot pause"
        );
    }

    #[tokio::test]
    async fn failure_keeps_reason_retry_clears_it() {
        let m = mgr();
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        m.mark_running(&t.id).await.unwrap();
        let t = m.fail(&t.id, "connection reset").await.unwrap();
        assert_eq!(t.status, TaskStatus::Failed);
        assert_eq!(t.error.as_deref(), Some("connection reset"));

        let t = m.resume(&t.id).await.unwrap();
        assert_eq!(t.status, TaskStatus::Queued);
        assert_eq!(t.error, None, "retry clears the stale reason");
    }

    #[tokio::test]
    async fn pause_queued_and_running_both_legal() {
        let m = mgr();
        let a = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        let a = m.pause(&a.id).await.unwrap();
        assert_eq!(a.status, TaskStatus::Paused);
        let a = m.resume(&a.id).await.unwrap();
        assert_eq!(a.status, TaskStatus::Queued);

        let b = m
            .add("http://x/b", "/tmp/b", Priority::Normal)
            .await
            .unwrap();
        m.mark_running(&b.id).await.unwrap();
        let b = m.pause(&b.id).await.unwrap();
        assert_eq!(b.status, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn illegal_transitions_refused_with_detail() {
        let m = mgr();
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        // Queued -> Completed: never (must run first).
        assert!(matches!(
            m.complete(&t.id).await,
            Err(TaskError::IllegalTransition {
                from: TaskStatus::Queued,
                to: TaskStatus::Completed,
                ..
            })
        ));
        // Queued -> Failed: never.
        assert!(matches!(
            m.fail(&t.id, "x").await,
            Err(TaskError::IllegalTransition {
                from: TaskStatus::Queued,
                to: TaskStatus::Failed,
                ..
            })
        ));
        // Paused -> Completed: never.
        m.pause(&t.id).await.unwrap();
        assert!(matches!(
            m.complete(&t.id).await,
            Err(TaskError::IllegalTransition {
                from: TaskStatus::Paused,
                ..
            })
        ));
        // State survived the refused calls.
        assert_eq!(
            m.get(&t.id).await.unwrap().unwrap().status,
            TaskStatus::Paused
        );
    }

    #[tokio::test]
    async fn resume_on_running_refused() {
        // (Running, Queued) is not user-reachable: an engine is still
        // downloading; re-queueing under it double-starts the task.
        let m = mgr();
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        m.mark_running(&t.id).await.unwrap();
        assert!(matches!(
            m.resume(&t.id).await,
            Err(TaskError::IllegalTransition {
                from: TaskStatus::Running,
                to: TaskStatus::Queued,
                ..
            })
        ));
        assert_eq!(
            m.get(&t.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn fail_on_paused_refused() {
        let m = mgr();
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        m.pause(&t.id).await.unwrap();
        assert!(matches!(
            m.fail(&t.id, "late engine error").await,
            Err(TaskError::IllegalTransition {
                from: TaskStatus::Paused,
                to: TaskStatus::Failed,
                ..
            })
        ));
        assert_eq!(
            m.get(&t.id).await.unwrap().unwrap().status,
            TaskStatus::Paused
        );
    }

    #[tokio::test]
    async fn update_progress_on_missing_or_done_is_silent() {
        let m = mgr();
        // Missing id: not an error (ticks race removal by design).
        m.update_progress(&TaskId::new("ghost"), 1, Some(2))
            .await
            .unwrap();
        // Completed task: late tick dropped, not stored.
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        m.mark_running(&t.id).await.unwrap();
        m.update_progress(&t.id, 100, None).await.unwrap();
        m.complete(&t.id).await.unwrap();
        m.update_progress(&t.id, 500, None).await.unwrap();
        let stored = m.get(&t.id).await.unwrap().unwrap();
        assert_eq!(stored.received_bytes, 100);
    }

    /// The CAS guard this crate exists for: two concurrent legal
    /// transitions from the same predecessor — exactly one wins,
    /// the loser gets a precise IllegalTransition. Deterministic
    /// regardless of scheduling (both validate Running; the SQL
    /// `WHERE status = ?from` serializes them).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_transitions_exactly_one_wins() {
        let m = Arc::new(mgr());
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        m.mark_running(&t.id).await.unwrap();

        let a = m.clone();
        let id_a = t.id.clone();
        let pauser = tokio::spawn(async move { a.pause(&id_a).await });
        let b = m.clone();
        let id_b = t.id.clone();
        let completer = tokio::spawn(async move { b.complete(&id_b).await });

        let (paused, completed) = tokio::join!(pauser, completer);
        let winners = [paused.unwrap().is_ok(), completed.unwrap().is_ok()]
            .iter()
            .filter(|w| **w)
            .count();
        assert_eq!(winners, 1, "exactly one transition may land");
        // And the row is in one of the two states, never both.
        let status = m.get(&t.id).await.unwrap().unwrap().status;
        assert!(matches!(status, TaskStatus::Paused | TaskStatus::Completed));
    }

    #[tokio::test]
    async fn remove_event_carries_row_snapshot() {
        let m = mgr();
        let mut rx = m.bus.subscribe();
        let t = m
            .add("http://x/snap", "/tmp/snap.bin", Priority::Normal)
            .await
            .unwrap();
        m.remove(&t.id).await.unwrap();
        match &drain(&mut rx)[1] {
            EngineEvent::TaskRemoved { id, url, save_path } => {
                assert_eq!(*id, t.id);
                assert_eq!(url, "http://x/snap");
                assert_eq!(save_path, "/tmp/snap.bin");
            }
            other => panic!("expected TaskRemoved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_id_is_not_found() {
        let m = mgr();
        let ghost = TaskId::new("nope");
        assert!(matches!(m.pause(&ghost).await, Err(TaskError::NotFound(_))));
        assert!(matches!(
            m.remove(&ghost).await,
            Err(TaskError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn remove_deletes_and_emits_any_status() {
        let m = mgr();
        let mut rx = m.bus.subscribe();
        let t = m
            .add("http://x/a", "/tmp/a", Priority::Normal)
            .await
            .unwrap();
        m.remove(&t.id).await.unwrap();
        assert!(m.get(&t.id).await.unwrap().is_none());
        assert!(
            !m.list(None).await.unwrap().iter().any(|x| x.id == t.id),
            "list must not resurrect removed rows"
        );
        assert!(matches!(
            &drain(&mut rx)[1],
            EngineEvent::TaskRemoved { .. }
        ));
    }

    #[tokio::test]
    async fn boot_recovers_running_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.db");
        let bus = EventBus::new(64);
        let id = {
            let m = TaskManager::new(Store::open(&path).unwrap(), bus.clone());
            let t = m
                .add("http://x/a", "/tmp/a", Priority::Normal)
                .await
                .unwrap();
            m.mark_running(&t.id).await.unwrap();
            m.update_progress(&t.id, 512, Some(2048)).await.unwrap();
            t.id
        };
        // Daemon "crashes": a new manager over the same database.
        let m = TaskManager::new(Store::open(&path).unwrap(), bus);
        assert_eq!(m.boot().await.unwrap(), 1);
        let t = m.get(&id).await.unwrap().unwrap();
        assert_eq!(t.status, TaskStatus::Queued);
        // Progress survives the crash: engine resume state continues
        // from here, not from zero.
        assert_eq!(t.received_bytes, 512);
        assert_eq!(t.total_bytes, Some(2048));
        assert_eq!(
            m.get(&id).await.unwrap().unwrap().status,
            TaskStatus::Queued
        );
        // Second boot is a no-op.
        assert_eq!(m.boot().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn fresh_ids_are_unique_and_ordered() {
        let a = fresh_task_id();
        let b = fresh_task_id();
        let c = fresh_task_id();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert!(a.as_str() < b.as_str(), "lexicographic = chronological");
        assert!(b.as_str() < c.as_str());
    }

    /// The manager holds no task state, so parallel lifecycle calls
    /// must serialize correctly through the store alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_adds_all_persist() {
        let m = Arc::new(mr());
        let mut handles = Vec::new();
        for i in 0..25 {
            let m = m.clone();
            handles.push(tokio::spawn(async move {
                m.add(format!("http://x/{i}"), "/tmp/x", Priority::Normal)
                    .await
                    .unwrap()
                    .id
            }));
        }
        let ids: Vec<TaskId> = futures_shim(handles).await;
        assert_eq!(ids.len(), 25);
        assert_eq!(
            ids.len(),
            ids.clone()
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        assert_eq!(m.list(None).await.unwrap().len(), 25);
    }

    fn mr() -> TaskManager {
        mgr()
    }

    async fn futures_shim(handles: Vec<tokio::task::JoinHandle<TaskId>>) -> Vec<TaskId> {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    }

    /// Silence the unused warning for ProgressSink import kept as a
    /// deliberate reminder that progress flows engine->bus, not here.
    #[allow(dead_code)]
    fn _sink_type_check(_: Arc<dyn ProgressSink>) {}
}
