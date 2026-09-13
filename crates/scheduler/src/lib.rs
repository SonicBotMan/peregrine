//! Task scheduler (M2-c, PROPOSAL §3.2 `scheduler/`): owns WHEN tasks
//! run and WHAT happens when their engine call returns.
//!
//! Three concerns, deliberately kept apart:
//! - [`peregrine_task_manager::TaskManager`] owns task STATE (the
//!   state machine, the durable queue, crash recovery) — the
//!   scheduler never writes status directly, only through it.
//! - The [`DownloadPort`] owns HOW bytes move (probe, route,
//!   single-stream vs segmented, resume) — the scheduler never sees
//!   HTTP. Production wiring implements it over
//!   `HttpEngine::download_auto`; tests use scripted fakes.
//! - [`Scheduler`] owns POLICY: the concurrency budget (`max_concurrent`
//!   tasks across ALL engines), priority-ordered dequeue, pause /
//!   resume / remove / shutdown, and result mapping — including the
//!   rule that `ApiError::Cancelled` is not a failure (M2-b).
//!
//! ## Result mapping (the contract with M2-b)
//!
//! ```text
//! engine Ok        → flush(final cumulative) → complete()   [CAS]
//! engine Cancelled → flush(last partial), NO terminal write
//!                    (pause()/shutdown() already moved the state
//!                    machine; writing again would race)
//! engine Err(e)    → flush(last partial) → fail(e)           [CAS]
//! ```
//! Every terminal write goes through the task manager's
//! compare-and-set, so a task removed or paused mid-flight resolves
//! to exactly one winner — stale worker outcomes are dropped as
//! `NotFound`/`IllegalTransition`, never overwrite a newer truth.
//!
//! ## Progress bridging (B1)
//!
//! The engine-side sink coalesces: every reported frame lands in a
//! shared cell, and a drainer task forwards at most one store
//! write + bus event per `progress_interval` (the final reading
//! always lands). Coalescing lives at the producer boundary, not in
//! the bus — a flooding engine can never overflow the 1024-slot bus
//! through us.

use peregrine_api::bus::{EngineEvent, EventBus};
use peregrine_api::download::{DownloadJob, DownloadOutcome, ProgressSink, SharedProgressSink};
use peregrine_api::error::ApiError;
use peregrine_api::task::{Priority, Task, TaskId, TaskStatus};
use peregrine_storage::Store;
use peregrine_task_manager::{TaskError, TaskManager};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// The engine boundary the scheduler talks to. One method: hand it a
/// fully-formed job (the scheduler derives resume context from disk
/// state), get an outcome or an `ApiError` (`Cancelled` = paused, not
/// failed — see the module docs).
mod hls_port;
pub use hls_port::HlsAutoPort;

pub trait DownloadPort: Send + Sync {
    fn auto_download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>>;

    /// Drop any engine-side resume state for (url, sink) — the
    /// segment rows / cursors a previous run left behind (B31).
    /// Called after a task row is removed so the SAME target can be
    /// re-added fresh: without this, `download_auto` route 1 sees a
    /// stale "live segments" row for a file the user deleted and
    /// resumes into a sparse mismatch. Failures are logged by the
    /// caller and never block the removal — a stale resume row is a
    /// degraded next-download, not a lost one.
    fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    /// Live-poke a running download's per-task rate limit
    /// (`None` = unlimited). Sync because implementations only flip
    /// atomics. Default no-op: scripted/test ports don't throttle;
    /// production is `HttpAutoPort`'s registry poked in-place.
    fn set_task_limit(&self, _url: &str, _sink: &std::path::Path, _bps: Option<u64>) {}
}

/// Production port over `HttpEngine::download_auto` (PROPOSAL §5:
/// probe → route → single/segmented with structured downgrade). Lives
/// here (not in engine-http) so the engine crate stays free of
/// scheduler concepts — orphan rules allow a local trait over a
/// foreign type.
pub struct HttpAutoPort {
    engine: Arc<peregrine_engine_http::HttpEngine>,
    cfg: peregrine_engine_http::SegmentConfig,
    store: Store,
    /// Daemon-wide byte budget (M3-b): every task's engine consults
    /// it in addition to its own per-task budget. Live-updatable via
    /// `set_bps` — no restart needed.
    global: peregrine_api::budget::SharedRateBudget,
    /// Per-task budgets handed out per download; keyed by (url,
    /// sink) because that is the identity the port sees. A running
    /// task's budget can be poked live by the settings layer.
    locals: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                (String, std::path::PathBuf),
                peregrine_api::budget::SharedRateBudget,
            >,
        >,
    >,
}

impl HttpAutoPort {
    pub fn new(
        engine: Arc<peregrine_engine_http::HttpEngine>,
        cfg: peregrine_engine_http::SegmentConfig,
        store: Store,
        global: peregrine_api::budget::SharedRateBudget,
    ) -> Self {
        Self {
            engine,
            cfg,
            store,
            global,
            locals: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Resolve the task's budget chain. `initial_bps` comes from the
    /// task ROW (0 = unlimited): a fresh session starts from the
    /// persisted limit, not from "unlimited" — live `set_task_limit`
    /// only refines an existing entry.
    fn budget_for(
        &self,
        url: &str,
        sink: &std::path::Path,
        initial_bps: u64,
    ) -> peregrine_api::budget::BudgetChain {
        let key = (url.to_string(), sink.to_path_buf());
        let local = {
            let mut map = self.locals.lock().expect("locals map poisoned");
            map.entry(key)
                .or_insert_with(|| peregrine_api::budget::RateBudget::with_bps(initial_bps))
                .clone()
        };
        peregrine_api::budget::BudgetChain {
            local,
            global: self.global.clone(),
        }
    }
}

impl DownloadPort for HttpAutoPort {
    fn auto_download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        let key = (job.url.clone(), job.sink.clone());
        let locals = self.locals.clone();
        Box::pin(async move {
            // Read the downloads ROW before resolving the budget:
            // the persisted limit seeds this session's local bucket.
            // (The engine `tasks` table has no limit — downloads is
            // the source of truth.)
            let row = async {
                let tid = self
                    .store
                    .find_active_download_by_target(&job.url, &job.sink.to_string_lossy())
                    .await
                    .ok()??;
                self.store.get_download(&tid).await.ok().flatten()
            }
            .await;
            let initial_bps = row.map(|t| t.speed_limit_bps).unwrap_or(0);
            let budget = self.budget_for(&job.url, job.sink.as_path(), initial_bps);
            let result = self
                .engine
                .download_auto(job, &self.cfg, &self.store, progress, cancel, &budget)
                .await;
            // The download (any route) is over — drop the registry
            // entry so a future re-add of the same target starts
            // from the row's limit, not this session's stale budget.
            if let Ok(mut map) = locals.lock() {
                map.remove(&key);
            }
            result
        })
    }

    fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        // Own the inputs before the async block: the elided
        // param lifetimes and the return `'_` can disagree when the
        // block captures them by reference.
        let url = url.to_string();
        let sink = sink.to_path_buf();
        // Engine rows are keyed (url, sink); segment rows cascade on
        // task delete (storage invariant, tested there).
        Box::pin(async move {
            if let Some(state) = self.store.get_task(&url, &sink).await? {
                self.store.delete_task(state.id).await?;
            }
            Ok(())
        })
    }

    fn set_task_limit(&self, url: &str, sink: &std::path::Path, bps: Option<u64>) {
        // Live poke: the RUNNING task's budget adapts on its next
        // byte; queued tasks read the row when they start.
        let key = (url.to_string(), sink.to_path_buf());
        if let Ok(map) = self.locals.lock()
            && let Some(b) = map.get(&key)
        {
            b.set_bps(bps.unwrap_or(0));
        }
    }
}

/// Policy knobs. Defaults mirror PROPOSAL §3.2 (a desktop daemon:
/// small enough to be polite, big enough to use the pipe).
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Max tasks in `Running` state at once, across all engines.
    pub max_concurrent: usize,
    /// Progress coalescing floor: one store write + bus event per
    /// task per interval (the final reading always lands).
    pub progress_interval: Duration,
    /// Poll floor: even with no events, re-check the queue this often
    /// (defends against a lost wakeup more than it drives progress).
    pub poll_interval: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            progress_interval: Duration::from_millis(250),
            poll_interval: Duration::from_millis(500),
        }
    }
}

struct Running {
    /// Cancel this to stop the task's engine call (pause/remove/
    /// shutdown). The worker maps the resulting `Cancelled` to
    /// "already transitioned, do nothing".
    cancel: CancellationToken,
    /// The file this worker is writing — the claim loop's
    /// path-level mutex key (two engines on one file corrupt it).
    save_path: String,
}

pub struct Scheduler {
    tm: Arc<TaskManager>,
    /// Daemon-wide byte budget (M3-b): the `global` half of every
    /// task's `BudgetChain`. Owned here so `set_global_limit` can
    /// persist (settings KV) AND apply (`set_bps`) in one call.
    global: peregrine_api::budget::SharedRateBudget,
    bus: EventBus,
    port: Arc<dyn DownloadPort>,
    cfg: SchedulerConfig,
    running: Mutex<HashMap<TaskId, Arc<Running>>>,
    /// Set by shutdown(); the run loop exits and every worker's
    /// token is cancelled.
    shutdown: CancellationToken,
    /// Run-loop wakeup: workers finishing, tasks added, slots freed.
    wake: Arc<Notify>,
}

impl Scheduler {
    pub fn new(
        tm: Arc<TaskManager>,
        bus: EventBus,
        port: Arc<dyn DownloadPort>,
        global: peregrine_api::budget::SharedRateBudget,
        cfg: SchedulerConfig,
    ) -> Self {
        assert!(cfg.max_concurrent >= 1, "max_concurrent must be >= 1");
        // Normalize degenerate intervals: a zero poll/progress
        // interval would busy-loop the drainer or the run loop
        // (R2 review nit). One millisecond is the honest floor.
        let cfg = SchedulerConfig {
            max_concurrent: cfg.max_concurrent,
            progress_interval: cfg.progress_interval.max(Duration::from_millis(1)),
            poll_interval: cfg.poll_interval.max(Duration::from_millis(1)),
        };
        Self {
            tm,
            bus,
            port,
            global,
            cfg,
            running: Mutex::new(HashMap::new()),
            shutdown: CancellationToken::new(),
            wake: Arc::new(Notify::new()),
        }
    }

    /// Access the task manager for CLI/IPC surfaces (list, get).
    pub fn tasks(&self) -> &TaskManager {
        &self.tm
    }

    /// Enqueue a new download through the scheduler facade (the ONLY
    /// sanctioned add path in a composed daemon: enqueue + wake in
    /// one step, so a task can never sit queued behind a sleeping
    /// loop).
    pub async fn add(
        &self,
        url: impl Into<String>,
        save_path: impl Into<String>,
        priority: Priority,
    ) -> Result<Task, TaskError> {
        let task = self.tm.add(url, save_path, priority).await?;
        self.wake.notify_one();
        Ok(task)
    }

    /// The main loop: top the queue up to the concurrency budget,
    /// forever, until [`Scheduler::shutdown`] is called. Event-driven
    /// with a poll floor — every state change notifies `wake`, and a
    /// worker finishing ALWAYS frees a slot, so the loop keeps
    /// draining without timers (they only bound latency after a
    /// hypothetical lost wakeup).
    pub async fn run(self: Arc<Self>) {
        loop {
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(self.cfg.poll_interval) => {}
            }
            self.fill_slots().await;
        }
        self.drain().await;
    }

    /// Pause a task: state first (`Running → Paused`, CAS), THEN
    /// cancel the engine. The order is load-bearing: the engine's
    /// `Cancelled` return maps to "do nothing" precisely because the
    /// state machine has already moved. A queued task needs no token
    /// at all (it never started).
    pub async fn pause(&self, id: &TaskId) -> Result<Task, TaskError> {
        let task = self.tm.pause(id).await?;
        if let Some(r) = self.running.lock().unwrap().get(id) {
            r.cancel.cancel();
        }
        // A paused head-of-queue changes what fill_slots should pick
        // next — wake so the policy re-evaluates immediately.
        self.wake.notify_one();
        Ok(task)
    }

    /// Set the daemon-wide rate limit (0 = unlimited): persist to
    /// the settings KV (survives restarts) and apply to the live
    /// budget (running engines feel it on their next byte).
    pub async fn set_global_limit(&self, bps: u64) -> Result<(), TaskError> {
        self.tm
            .store()
            .set_setting("global_limit_bps", &bps.to_string())
            .await?;
        self.global.set_bps(bps);
        Ok(())
    }

    /// Restore the persisted global limit at boot (settings KV →
    /// live budget). Missing key = unlimited, the default.
    pub async fn restore_global_limit(&self) -> Result<u64, TaskError> {
        let raw = self.tm.store().get_setting("global_limit_bps").await?;
        let bps = raw.and_then(|s| s.parse().ok()).unwrap_or(0);
        self.global.set_bps(bps);
        Ok(bps)
    }

    /// Set a task's rate limit (0 = unlimited): persist via the
    /// manager (queued tasks pick it up at spawn) and poke a
    /// RUNNING engine's bucket in-place — the next byte pays the new
    /// rate, no restart.
    pub async fn set_task_limit(&self, id: &TaskId, bps: u64) -> Result<Task, TaskError> {
        let task = self.tm.set_limit(id, bps).await?;
        self.port.set_task_limit(
            &task.url,
            task.save_path.as_ref(),
            (bps != 0).then_some(bps),
        );
        Ok(task)
    }

    /// Resume a paused task: `Paused → Queued` (CAS), then wake the
    /// loop — the next `fill_slots` picks it up by priority.
    pub async fn resume(&self, id: &TaskId) -> Result<Task, TaskError> {
        let task = self.tm.resume(id).await?;
        self.wake.notify_one();
        Ok(task)
    }

    /// Remove a task. Idempotent (R2 review): a second remove — or
    /// one racing the worker's terminal write — reports success, so
    /// IPC callers can retry safely instead of parsing `NotFound`.
    /// Partial files stay on disk — deleting user data is the
    /// caller's (IPC command's) explicit choice, not the scheduler's
    /// default.
    pub async fn remove(&self, id: &TaskId) -> Result<(), TaskError> {
        // Row first (url/sink needed for the engine purge below).
        let Some(row) = self.tm.get(id).await? else {
            return Ok(()); // idempotent (R2 review)
        };
        if let Some(r) = self.running.lock().unwrap().get(id) {
            r.cancel.cancel();
        }
        self.tm.remove(id).await?;
        // Engine-side resume rows (B31): stale segment rows keyed
        // (url, sink) must not outlive the task — a re-add of the
        // same target would resume into a mismatched sparse file.
        // Best-effort: failure logs and never blocks removal.
        if let Err(e) = self
            .port
            .purge(&row.url, std::path::Path::new(&row.save_path))
            .await
        {
            tracing::warn!(task = %id, error = %e, "engine purge after remove failed");
        }
        // Removing a running task frees a slot; removing a queued one
        // changes the pick order. Either way, wake.
        self.wake.notify_one();
        Ok(())
    }

    /// Graceful shutdown: stop accepting work, cancel every worker,
    /// wait for the last one to finish. Workers map `Cancelled` to a
    /// no-op state write, so interrupted tasks sit in whatever
    /// non-terminal state `pause`/removal left them — and a task
    /// cancelled mid-engine still `Running` is re-queued by the next
    /// boot's crash recovery. The durable queue IS the shutdown state.
    pub async fn shutdown(&self) -> usize {
        self.shutdown.cancel();
        self.wake.notify_one();
        let pendings: Vec<Arc<Running>> = self.running.lock().unwrap().values().cloned().collect();
        for r in pendings {
            r.cancel.cancel();
        }
        self.drain().await
    }

    /// Pull queued tasks (priority first) until the budget is full.
    /// Stops immediately once shutdown fires (R2 review: a mid-pass
    /// `fill_slots` would otherwise keep flipping never-started
    /// tasks to `Running` — churn whose biased-select workers then
    /// resolve Cancelled without a terminal write, leaving rows
    /// `Running` until the next boot).
    async fn fill_slots(self: &Arc<Self>) {
        loop {
            if self.shutdown.is_cancelled() {
                return;
            }
            let free = self
                .cfg
                .max_concurrent
                .saturating_sub(self.running.lock().unwrap().len());
            if free == 0 {
                return;
            }
            let Some(task) = self.claim_next_queued().await else {
                return; // queue empty (or every candidate blocked)
            };
            self.spawn_worker(task);
        }
    }

    /// Highest-priority queued task, claimed via CAS `Queued →
    /// Running`. A lost CAS (task paused/removed between the list
    /// and the claim) falls through to the NEXT candidate instead of
    /// abandoning the whole pass (R2 review: the old code skipped
    /// every other runnable task until the next wake/poll floor).
    /// A candidate whose `save_path` is already being written by a
    /// running task is skipped (path-level mutual exclusion — two
    /// engines on one file corrupt it); the next wake/poll pass
    /// retries it once the path frees up.
    async fn claim_next_queued(&self) -> Option<Task> {
        let queued = match self.tm.list(Some(TaskStatus::Queued)).await {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "listing queued tasks failed");
                return None;
            }
        };
        // list() order is unspecified; the scheduler's policy is
        // priority, then FIFO (created_at; ties resolved by list
        // order, which the store sorts chronologically by id).
        let mut candidates: Vec<Task> = queued;
        candidates.sort_by_key(|t| (std::cmp::Reverse(t.priority), t.created_at));
        // Paths currently being written — path-level mutual
        // exclusion (R2 P1: two engines on one file corrupt it; the
        // (url, path) duplicate is already rejected at add(), this
        // catches same-path-different-url).
        let busy_paths: Vec<String> = self
            .running
            .lock()
            .unwrap()
            .values()
            .map(|r| r.save_path.clone())
            .collect();
        for pick in candidates {
            if busy_paths.contains(&pick.save_path) {
                continue; // retry on a later wake, path is busy
            }
            match self.tm.mark_running(&pick.id).await {
                Ok(task) => return Some(task),
                Err(TaskError::NotFound(_) | TaskError::IllegalTransition { .. }) => continue,
                Err(e) => {
                    tracing::error!(error = %e, "claim failed");
                    return None;
                }
            }
        }
        None // queue drained (or every remaining candidate path-blocked)
    }

    /// Spawn one worker for a claimed (already `Running`) task.
    /// The JoinHandle is supervised (R2 review F8): a panicking
    /// worker's future never returns, so the slot frees via
    /// `WorkerGuard` but the panic itself would be invisible and the
    /// row would sit `Running` forever — the supervisor awaits the
    /// handle, logs, and moves the row to `Failed`.
    fn spawn_worker(self: &Arc<Self>, task: Task) {
        let running = Arc::new(Running {
            cancel: CancellationToken::new(),
            save_path: task.save_path.clone(),
        });
        self.running
            .lock()
            .unwrap()
            .insert(task.id.clone(), running.clone());

        let sched = Arc::clone(self);
        let tm = Arc::clone(&self.tm);
        let id = task.id.clone();
        let handle = tokio::spawn(async move { Worker::new(sched, task, running).run().await });
        tokio::spawn(async move {
            if let Err(panic) = handle.await {
                tracing::error!(task = %id, panic = ?panic, "worker PANICKED");
                // Best-effort terminal write so the row does not sit
                // `Running` until the next boot; a lost CAS here
                // (row already moved) is fine.
                let _ = tm.fail(&id, "internal error: worker panicked").await;
            }
        });
    }

    /// Wait for every spawned worker to finish, returning the number
    /// still in flight when we gave up (0 = fully drained). Polled on
    /// a short interval with an explicit deadline (R2 review):
    /// sharing the run-loop's `wake` Notify made drain latency depend
    /// on which waiter won the permit (flaky), and a leaked slot
    /// would hang it forever. Callers that need "no engine is writing
    /// files" (DB compaction, file moves, process exit) must check
    /// the return — the deadline is a ceiling on the drain contract,
    /// not a guarantee. The durable queue is the shutdown truth
    /// either way (M2-a crash recovery re-queues whatever was still
    /// Running).
    async fn drain(&self) -> usize {
        const DRAIN_POLL: Duration = Duration::from_millis(20);
        const DRAIN_DEADLINE: Duration = Duration::from_secs(30);
        let started = std::time::Instant::now();
        loop {
            let stuck = self.running.lock().unwrap().len();
            if stuck == 0 {
                return 0;
            }
            if started.elapsed() >= DRAIN_DEADLINE {
                tracing::error!(
                    elapsed = ?started.elapsed(),
                    stuck,
                    "drain deadline exceeded; workers may still be in flight"
                );
                return stuck;
            }
            tokio::time::sleep(DRAIN_POLL).await;
        }
    }
}

/// Panic-safe teardown for one worker (R2 review, Race C): map-slot
/// removal + drainer stop happen in `Drop`, so a panicking `drive`
/// cannot leak a concurrency slot (which would wedge `drain`
/// forever). The happy path drops it right after `drive` returns —
/// the sink's own `finish*` calls already cancelled the drainer,
/// and `Drop::drop` cancelling again is a no-op.
struct WorkerGuard {
    sched: Arc<Scheduler>,
    id: TaskId,
    sink_stop: CancellationToken,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.sink_stop.cancel();
        self.sched.running.lock().unwrap().remove(&self.id);
        self.sched.wake.notify_one();
    }
}

// ---------------------------------------------------------------------
// Worker: one task's engine call + result mapping + progress drain.

struct Worker {
    sched: Arc<Scheduler>,
    task: Task,
    running: Arc<Running>,
}

impl Worker {
    fn new(sched: Arc<Scheduler>, task: Task, running: Arc<Running>) -> Self {
        Self {
            sched,
            task,
            running,
        }
    }

    async fn run(self) {
        let id = self.task.id.clone();
        // Seed the sink with the row's CURRENT cumulative reading
        // (M3-b1 smoke P0): a paused task's cursors lag the store's
        // `received_bytes` by up to one persist/progress quantum per
        // worker (tail bytes reported but not yet flushed). A sink
        // starting from 0 combined with on_progress's monotone max()
        // would freeze the row's reading at the OLD value until new
        // bytes out-ran the tail — at 128 KiB/s that's ~10 s of a
        // dead-looking live view, at tiny rates minutes. The fill
        // snapshot is fresh enough: between fill and this worker's
        // engine call no other worker writes progress for this row
        // (single-worker-per-task is the scheduler's invariant).
        let seed = self.task.received_bytes;
        let sink = CoalescingSink::new(
            self.sched.tm.clone(),
            self.sched.bus.clone(),
            id.clone(),
            self.sched.cfg.progress_interval,
            seed,
        );
        // Panic-safe slot release (R2 review, Race C): the guard
        // removes the map entry and stops the sink drainer even if
        // `drive` panics — a leaked slot would deadlock
        // `shutdown`'s drain forever otherwise.
        let guard = WorkerGuard {
            sched: Arc::clone(&self.sched),
            id: id.clone(),
            sink_stop: sink.stop_token(),
        };
        let result = self.drive(&sink).await;
        drop(guard);
        if let Err(e) = result {
            tracing::error!(task = %id, error = %e, "worker failed unexpectedly");
        }
    }

    /// The engine call and its result mapping. Terminal state writes
    /// use the task manager's CAS; every "lost" outcome
    /// (`NotFound`/`IllegalTransition`) means a newer truth already
    /// landed (pause/remove during the engine call) and is silently
    /// accepted — never an error.
    async fn drive(&self, sink: &Arc<CoalescingSink>) -> Result<(), TaskError> {
        // Close the claim/insert race (R2 review, Race A): if
        // pause()/remove() ran between the state machine's
        // `Queued → Running` CAS and the running-map insert, no
        // cancel token exists for them to find — verify the row
        // still says Running before touching the engine, else the
        // bytes would keep flowing against a paused/removed task.
        match self.sched.tm.get(&self.task.id).await {
            Ok(Some(t)) if t.status == TaskStatus::Running => {}
            Ok(_) => return Ok(()), // newer truth already landed
            Err(TaskError::NotFound(_)) => return Ok(()),
            Err(e) => {
                tracing::warn!(task = %self.task.id, error = %e, "pre-flight row check failed");
                return Err(e);
            }
        }

        let job = resume_job(&self.task).await;
        let resume_start = job.resume.as_ref().map(|r| r.start_offset).unwrap_or(0);

        // Race B (R2 review): a worker claimed after `shutdown()`
        // snapshotted the running map would never see its token
        // cancelled. Watch the scheduler-level shutdown directly —
        // dropping the engine future mid-flight is exactly what the
        // M2-b cancellation chain is built for.
        let outcome = tokio::select! {
            biased;
            _ = self.sched.shutdown.cancelled() => Err(ApiError::Cancelled),
            o = self.sched
                .port
                .auto_download(job, sink.shared(), self.running.cancel.clone()) => o,
        };

        match outcome {
            Ok(out) => {
                // `bytes_written` is session-only by contract; the
                // sink's cumulative reading already includes the
                // resume offset. Take the max of both: whichever
                // channel saw the last byte, the final store write
                // must not go backwards.
                Arc::clone(sink)
                    .finish(resume_start, out.bytes_written, out.total_bytes)
                    .await;
                // A lost CAS (task paused/removed in the window between
                // the engine's `Ok` and this write) is a newer truth
                // winning — not an error.
                match self.sched.tm.complete(&self.task.id).await {
                    Ok(_)
                    | Err(TaskError::NotFound(_))
                    | Err(TaskError::IllegalTransition { .. }) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Err(ApiError::Cancelled) => {
                // NOT a failure (M2-b): pause()/shutdown()/remove()
                // already moved the state machine. A pause left the
                // partial + cursors; nothing to write here. The last
                // reading still lands (progress writes are
                // status-guarded and idempotent).
                Arc::clone(sink).finish_pending().await;
                Ok(())
            }
            Err(e) => {
                tracing::warn!(task = %self.task.id, error = %e, "engine failed");
                Arc::clone(sink).finish_pending().await;
                match self.sched.tm.fail(&self.task.id, e.to_string()).await {
                    Ok(_)
                    | Err(TaskError::NotFound(_))
                    | Err(TaskError::IllegalTransition { .. }) => Ok(()),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

/// Derive the engine job from the task row. Resume context is
/// disk-derived (the truth), not row-derived (the row may lag the
/// engine's last flush by up to `progress_interval`):
/// - a sink file exists → resume from its current length
///   (`start_offset` = file len; engine re-verifies with Range/206).
/// - no file → fresh download. The segment store row (if any) is
///   discovered inside `download_auto` route 1.
async fn resume_job(task: &Task) -> DownloadJob {
    let sink = std::path::PathBuf::from(&task.save_path);
    let resume = match tokio::fs::metadata(&sink).await {
        Ok(m) if m.len() > 0 => Some(peregrine_api::download::ResumeContext {
            start_offset: m.len(),
            validator: None,
        }),
        _ => None,
    };
    DownloadJob {
        url: task.url.clone(),
        sink,
        resume,
        expected_total: task.total_bytes,
    }
}

// ---------------------------------------------------------------------
// Coalescing progress sink (B1): engine frames → shared cell →
// interval-paced store writes + bus events, final reading guaranteed.
//
// Why coalescing lives HERE and not in the bus: the bus is
// lossy-tolerant by design (UI progress), but the STORE must not see
// a write storm. The sink is the producer boundary — it is the only
// place that knows "this engine floods" and can shape the write
// rate before it hits anything shared.

struct CoalescingSink {
    tm: Arc<TaskManager>,
    bus: EventBus,
    id: TaskId,
    interval: Duration,
    /// Latest cumulative reading `(received, total)`. `max()` on
    /// every update keeps it monotone even if an engine ever
    /// reported a regression.
    state: Mutex<(u64, Option<u64>)>,
    /// Last value published to the BUS (monotone guard, R2 review):
    /// the store write is unconditional, but a late drainer tick
    /// must not emit a smaller `TaskProgress` after the final one.
    last_published: Mutex<(u64, Option<u64>)>,
    /// The engine-declared session base (see `on_session_base`),
    /// and the row reading at worker start (the seed). Re-basing
    /// every reading onto the seed (`seed + (v - base)`) keeps the
    /// column monotone from the first post-resume frame AND
    /// immediately responsive: a paused task's cursors lag the
    /// row's `received_bytes` by one persist/progress quantum per
    /// worker (the paused tail), and a monotone max() over raw
    /// engine absolutes would freeze the column until new bytes
    /// out-ran that tail (M3-b1 smoke P0: ~10 s of dead air at
    /// 128 KiB/s, minutes at tiny rates). No declared base (old
    /// engines, mocks) → raw absolutes, unchanged semantics.
    base: Mutex<Option<u64>>,
    seed: u64,
    /// New data since the last flush. Cleared before flushing; a
    /// concurrent frame re-arms it, so the next tick writes again —
    /// no frame is ever silently swallowed.
    dirty: AtomicBool,
    /// Stops the drainer. Cancelled by `finish`/`finish_pending` —
    /// the drainer never outlives the engine call.
    stop: CancellationToken,
}

impl CoalescingSink {
    fn new(
        tm: Arc<TaskManager>,
        bus: EventBus,
        id: TaskId,
        interval: Duration,
        seed_received: u64,
    ) -> Arc<Self> {
        let sink = Arc::new(Self {
            tm,
            bus,
            id,
            interval,
            state: Mutex::new((seed_received, None)),
            last_published: Mutex::new((seed_received, None)),
            base: Mutex::new(None),
            seed: seed_received,
            dirty: AtomicBool::new(false),
            stop: CancellationToken::new(),
        });
        sink.spawn_drainer();
        sink
    }

    fn shared(self: &Arc<Self>) -> SharedProgressSink {
        self.clone()
    }

    /// The drainer's stop token — the worker guard cancels this in
    /// `Drop`, so a panicking worker cannot leak its drainer task.
    fn stop_token(&self) -> CancellationToken {
        self.stop.clone()
    }
    fn spawn_drainer(self: &Arc<Self>) {
        let sink = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sink.stop.cancelled() => break,
                    _ = tokio::time::sleep(sink.interval) => {
                        if sink.dirty.swap(false, Ordering::AcqRel) {
                            let _ = sink.flush().await;
                        }
                    }
                }
            }
        });
    }

    /// One paced tick: swap the dirty flag FIRST (a frame arriving
    /// after the swap re-arms it), then snapshot and write. A frame
    /// landing between the snapshot and the write is picked up by
    /// the next tick — no reading is ever silently swallowed.
    ///
    /// Bus events are regression-guarded (R2 review): a drainer tick
    /// already past its `select!` when `finish` runs can publish a
    /// SMALLER reading after the final one — a raw-event UI would
    /// show progress going backwards after completion. The store
    /// write stays unconditional (its SQL is `MAX`-clamped); only
    /// the bus skip is monotone.
    async fn flush(&self) {
        let (received, total) = *self.state.lock().unwrap();
        if let Err(e) = self.tm.update_progress(&self.id, received, total).await {
            tracing::warn!(task = %self.id, error = %e, "progress flush failed");
        }
        {
            let mut lp = self.last_published.lock().unwrap();
            let regresses =
                received < lp.0 || lp.1.is_some_and(|known| total.is_some_and(|t| t < known));
            if regresses {
                return;
            }
            *lp = (received, total);
        }
        self.bus.publish(EngineEvent::TaskProgress {
            id: self.id.clone(),
            received,
            total,
        });
    }

    /// Map an engine-reported cumulative onto the seed base when
    /// the engine declared its session base; raw otherwise.
    fn rebase(&self, engine_value: u64) -> u64 {
        match *self.base.lock().unwrap() {
            Some(base) => engine_value.saturating_sub(base).saturating_add(self.seed),
            None => engine_value,
        }
    }

    /// Terminal path after `Ok`: fold in the session outcome, stop
    /// the drainer, land the final reading. The final write is the
    /// one that matters for crash recovery — it must happen even if
    /// the interval just ticked (hence a forced flush here, not just
    /// a dirty flag).
    ///
    /// `session_bytes` is THIS RUN's bytes (M2-b: engine reports
    /// session-only), so the row's cumulative = max(prior
    /// cumulative, resume_start + session). When the server told us
    /// the total, clamp to it — a 200-replay (server ignored Range,
    /// engine rewrote from zero) would otherwise double-count the
    /// stale resume offset.
    async fn finish(self: Arc<Self>, resume_start: u64, session_bytes: u64, total: Option<u64>) {
        {
            let mut st = self.state.lock().unwrap();
            st.0 =
                st.0.max(self.rebase(resume_start.saturating_add(session_bytes)));
            st.1 = total.or(st.1);
            if let Some(t) = st.1 {
                st.0 = st.0.min(t); // total is the authoritative ceiling
            }
        }
        self.stop.cancel();
        self.flush().await;
    }

    /// Terminal path after `Cancelled`/`Err`: land whatever the
    /// engine last reported (a paused task's row should reflect the
    /// partial progress it died with), then stop the drainer.
    async fn finish_pending(self: Arc<Self>) {
        self.stop.cancel();
        self.flush().await;
    }
}

impl ProgressSink for CoalescingSink {
    fn on_session_base(&self, base: u64) {
        *self.base.lock().unwrap() = Some(base);
    }

    /// Sync by trait contract (the engine's reporting surface is
    /// sync): stash the reading, mark dirty, return. No allocation,
    /// no I/O, no lock held across await — an engine may call this
    /// thousands of times per second and the cost stays O(1).
    fn on_progress(&self, p: &peregrine_api::download::DownloadProgress) {
        let mut st = self.state.lock().unwrap();
        // Contract tripwire (M3-b1 R2 P1-2): a reading BELOW the
        // seed with no declared base means this engine skipped
        // `on_session_base` — the rebase path is off and the
        // monotone max() will freeze the column until the session
        // out-runs the tail. Loud, not silent.
        if self.base.lock().unwrap().is_none() && p.bytes_done < self.seed {
            tracing::warn!(
                task = ?self.id,
                reading = p.bytes_done,
                seed = self.seed,
                "engine reported below-row reading without declaring a session base — \
                 received_bytes may freeze until the session out-runs the row"
            );
        }
        st.0 = st.0.max(self.rebase(p.bytes_done));
        // Total only ever GROWS (R2 review: `p.total.or(st.1)` would
        // let a later frame's smaller/absent total shrink it, and the
        // finish-time min-clamp would then clamp received below
        // what was already stored).
        st.1 = match (st.1, p.total) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, t) => t,
            (t, None) => t,
        };
        drop(st);
        self.dirty.store(true, Ordering::Release);
    }
}
