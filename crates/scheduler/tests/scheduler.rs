//! Scheduler integration tests (M2-c): POLICY, not bytes. The engine
//! is a scripted fake (`ScriptedPort`); the store/task-manager/bus
//! are real. What these tests pin down:
//!
//! 1. the concurrency budget + priority-ordered dequeue,
//! 2. the M2-b result-mapping contract (Ok → Complete, Err → Failed,
//!    Cancelled → "already transitioned, write nothing"),
//! 3. CAS discipline under racing outcomes (remove wins over a late
//!    engine `Ok`),
//! 4. progress coalescing at the sink (write storm in, interval-paced
//!    writes out, final reading always lands),
//! 5. disk-derived resume context,
//! 6. shutdown draining.

use peregrine_api::bus::{EngineEvent, EventBus};
use peregrine_api::download::{DownloadJob, DownloadOutcome, DownloadProgress, SharedProgressSink};
use peregrine_api::error::ApiError;
use peregrine_api::task::{Priority, Task, TaskId, TaskStatus};
use peregrine_scheduler::{DownloadPort, Scheduler, SchedulerConfig};
use peregrine_storage::Store;
use peregrine_task_manager::{TaskError, TaskManager};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// Scripted engine port.

#[derive(Debug, Clone)]
enum Script {
    /// Report `frames` monotone progress frames, then return `Ok`.
    Ok {
        bytes: u64,
        total: Option<u64>,
        frames: u64,
        /// Per-frame pause; `None` = yield only (fast flood).
        frame_pause: Option<Duration>,
    },
    /// Report one partial frame, then hang until cancelled →
    /// `Err(Cancelled)` (the M2-b pause contract).
    AwaitCancel { partial: u64, total: Option<u64> },
    /// Hang until cancelled, then STILL return `Ok` (a racing engine
    /// finish vs user remove — the CAS must drop this outcome).
    RaceOk { bytes: u64, total: Option<u64> },
    /// Return `Ok` with `replayed_from_zero = true` and no progress
    /// frames — the B34 200-replay outcome (server discarded the
    /// resume offset; `bytes_written` counts from ZERO).
    OkReplayed { bytes: u64, total: Option<u64> },
    /// Declare the session base (engine's `on_session_base`) and
    /// THEN return a B34 200-replay outcome — pins the interaction
    /// the real engine produces: base first, replay after.
    OkReplayedAfterBase {
        base: u64,
        bytes: u64,
        total: Option<u64>,
    },
    /// Fail immediately.
    Fail(ApiError),
    /// Declare a session base, then report `frames` frames
    /// COUNTED FROM that base (the real engines' post-resume
    /// behavior — readings are absolute cumulatives from their own
    /// resume base, which can LAG the row's received_bytes).
    OkFromBase {
        base: u64,
        bytes: u64,
        total: Option<u64>,
        frames: u64,
        /// Optional inter-frame pause (drives drainer flush
        /// granularity in speed-shape tests).
        frame_pause: Option<Duration>,
    },
    /// Panic inside the engine future (a misbehaving port): the
    /// supervisor must log, move the row to Failed, and free the
    /// slot (R2 review F8/F10).
    Panic,
}

struct ScriptedPort {
    scripts: Mutex<VecDeque<Script>>,
    jobs: Mutex<Vec<DownloadJob>>,
    active: AtomicUsize,
    peak_active: AtomicUsize,
    /// (url, sink) pairs the scheduler asked the engine to purge
    /// (B31: remove must drop engine-side resume rows).
    purged: Mutex<Vec<(String, std::path::PathBuf, bool)>>,
    /// (url, sink, bps) live limit pokes received (M3-b).
    limit_calls: Mutex<Vec<(String, std::path::PathBuf, Option<u64>)>>,
}

impl ScriptedPort {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            jobs: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            peak_active: AtomicUsize::new(0),
            purged: Mutex::new(Vec::new()),
            limit_calls: Mutex::new(Vec::new()),
        })
    }

    fn jobs(&self) -> Vec<DownloadJob> {
        self.jobs.lock().unwrap().clone()
    }

    fn purged(&self) -> Vec<(String, std::path::PathBuf, bool)> {
        self.purged.lock().unwrap().clone()
    }

    fn limit_calls(&self) -> Vec<(String, std::path::PathBuf, Option<u64>)> {
        self.limit_calls.lock().unwrap().clone()
    }
}

impl DownloadPort for ScriptedPort {
    fn auto_download(
        &self,
        job: DownloadJob,
        progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        self.jobs.lock().unwrap().push(job);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted port ran out of scripts");
        let active = &self.active;
        let peak = &self.peak_active;
        Box::pin(async move {
            let n = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            // Drop-guard the counter: the scheduler may DROP this
            // future mid-flight (shutdown select!), which must count
            // as "engine stopped" exactly like a graceful return.
            struct ActiveGuard<'a>(&'a AtomicUsize);
            impl Drop for ActiveGuard<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _guard = ActiveGuard(active);
            run_script(script, &progress, cancel).await
        })
    }

    fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
        purge_files: bool,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        self.purged
            .lock()
            .unwrap()
            .push((url.to_string(), sink.to_path_buf(), purge_files));
        Box::pin(async { Ok(()) })
    }

    fn set_task_limit(&self, url: &str, sink: &std::path::Path, bps: Option<u64>) {
        self.limit_calls
            .lock()
            .unwrap()
            .push((url.to_string(), sink.to_path_buf(), bps));
    }
}

fn outcome(bytes: u64, total: Option<u64>) -> DownloadOutcome {
    DownloadOutcome {
        bytes_written: bytes,
        total_bytes: total,
        completed: true,
        final_url: "http://test/file".into(),
        final_validator: None,
        replayed_from_zero: false,
    }
}

async fn run_script(
    s: Script,
    p: &SharedProgressSink,
    cancel: CancellationToken,
) -> Result<DownloadOutcome, ApiError> {
    match s {
        Script::Ok {
            bytes,
            total,
            frames,
            frame_pause,
        } => {
            for i in 1..=frames {
                p.on_progress(&DownloadProgress {
                    bytes_done: bytes * i / frames.max(1),
                    total,
                });
                match frame_pause {
                    Some(d) => tokio::time::sleep(d).await,
                    None => tokio::task::yield_now().await,
                }
            }
            Ok(outcome(bytes, total))
        }
        Script::AwaitCancel { partial, total } => {
            p.on_progress(&DownloadProgress {
                bytes_done: partial,
                total,
            });
            cancel.cancelled().await;
            Err(ApiError::Cancelled)
        }
        Script::RaceOk { bytes, total } => {
            cancel.cancelled().await;
            Ok(outcome(bytes, total))
        }
        Script::OkFromBase {
            base,
            bytes,
            total,
            frames,
            frame_pause,
        } => {
            p.on_session_base(base);
            for i in 1..=frames {
                p.on_progress(&DownloadProgress {
                    bytes_done: base + bytes * i / frames.max(1),
                    total,
                });
                match frame_pause {
                    Some(d) => tokio::time::sleep(d).await,
                    None => tokio::task::yield_now().await,
                }
            }
            Ok(outcome(bytes, total))
        }
        Script::OkReplayed { bytes, total } => {
            let mut o = outcome(bytes, total);
            o.replayed_from_zero = true;
            Ok(o)
        }
        Script::OkReplayedAfterBase { base, bytes, total } => {
            p.on_session_base(base);
            let mut o = outcome(bytes, total);
            o.replayed_from_zero = true;
            Ok(o)
        }
        Script::Fail(e) => Err(e),
        Script::Panic => {
            panic!("scripted port panic");
        }
    }
}

// ---------------------------------------------------------------------
// Harness.

struct Rig {
    sched: Arc<Scheduler>,
    port: Arc<ScriptedPort>,
    bus: EventBus,
}

fn rig(scripts: Vec<Script>, max_concurrent: usize) -> Rig {
    rig_cfg(
        scripts,
        SchedulerConfig {
            max_concurrent,
            progress_interval: Duration::from_millis(20),
            poll_interval: Duration::from_millis(10),
        },
    )
}

fn rig_cfg(scripts: Vec<Script>, cfg: SchedulerConfig) -> Rig {
    let bus = EventBus::new(1024);
    let tm = Arc::new(TaskManager::new(Store::open_memory().unwrap(), bus.clone()));
    let port = ScriptedPort::new(scripts);
    let sched = Arc::new(Scheduler::new(
        tm,
        bus.clone(),
        port.clone(),
        peregrine_api::budget::RateBudget::unlimited(),
        cfg,
    ));
    Rig { sched, port, bus }
}

/// Poll an async predicate every 10 ms until it holds; panic after
/// ~3 s with `what` as the message.
async fn wait_for<'a>(
    what: &str,
    mut pred: impl FnMut() -> Pin<Box<dyn Future<Output = bool> + 'a>>,
) {
    for _ in 0..300 {
        if pred().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never met within 3s: {what}");
}

async fn status_is(sched: &Scheduler, id: &TaskId, want: TaskStatus) -> bool {
    matches!(sched.tasks().get(id).await, Ok(Some(t)) if t.status == want)
}

fn abs_path(dir: &std::path::Path, name: &str) -> String {
    dir.join(name).to_string_lossy().into_owned()
}

async fn add(s: &Scheduler, dir: &std::path::Path, name: &str, p: Priority) -> Task {
    s.add("http://test/file", abs_path(dir, name), p)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------
// 1. Happy path.

#[tokio::test]
async fn remove_purge_flag_flows_to_engine_port() {
    // M5.1 P0-2: `purge=true` must reach the engine port as
    // `purge_files=true` — data deletion is the caller's explicit
    // choice, threaded REST→scheduler→port→engine verbatim.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::AwaitCancel {
            partial: 10,
            total: Some(100),
        }],
        2,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("engine entered", || {
        Box::pin(async { rig.port.peak_active.load(Ordering::SeqCst) >= 1 })
    })
    .await;

    rig.sched.remove(&t.id, true).await.unwrap();
    let purged = rig.port.purged();
    assert_eq!(purged.len(), 1);
    assert!(purged[0].2, "purge=true must reach the port");

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn remove_purges_engine_rows_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::AwaitCancel {
            partial: 10,
            total: Some(100),
        }],
        2,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("engine entered", || {
        Box::pin(async { rig.port.peak_active.load(Ordering::SeqCst) >= 1 })
    })
    .await;

    rig.sched.remove(&t.id, false).await.unwrap();
    // Engine-side resume rows dropped for exactly (url, sink) (B31).
    let purged = rig.port.purged();
    assert_eq!(
        purged,
        vec![(
            "http://test/file".to_string(),
            dir.path().join("f.bin"),
            false
        )]
    );

    // Idempotent: a second remove succeeds and does NOT re-purge
    // (the row is gone; nothing to purge).
    rig.sched.remove(&t.id, false).await.unwrap();
    assert_eq!(rig.port.purged().len(), 1);

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn completes_a_task_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::Ok {
            bytes: 1000,
            total: Some(1000),
            frames: 10,
            frame_pause: None,
        }],
        2,
    );
    let mut rx = rig.bus.subscribe();
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("task completes", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;

    // Final cumulative reading landed in the row (fresh download: no
    // resume offset, so session bytes == row bytes).
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(row.received_bytes, 1000);
    assert_eq!(row.total_bytes, Some(1000));

    // The job carried no resume context (no file existed yet).
    let jobs = rig.port.jobs();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].resume.is_none());

    // Bus saw terminal progress + completion.
    let mut saw_progress = false;
    let mut saw_completed = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            EngineEvent::TaskProgress { received: 1000, .. } => saw_progress = true,
            EngineEvent::TaskCompleted { .. } => saw_completed = true,
            _ => {}
        }
    }
    assert!(saw_progress, "final progress event must land");
    assert!(saw_completed);

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 2. Concurrency budget.

#[tokio::test]
async fn concurrency_budget_is_enforced_and_frees_up() {
    let dir = tempfile::tempdir().unwrap();
    // Two hanging tasks (await cancel), one that must wait.
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 10,
                total: Some(100),
            },
            Script::AwaitCancel {
                partial: 20,
                total: Some(100),
            },
            Script::Ok {
                bytes: 50,
                total: Some(50),
                frames: 1,
                frame_pause: None,
            },
        ],
        2,
    );
    tokio::spawn(rig.sched.clone().run());

    let _a = add(&rig.sched, dir.path(), "a", Priority::Normal).await;
    let b = add(&rig.sched, dir.path(), "b", Priority::Normal).await;
    let c = add(&rig.sched, dir.path(), "c", Priority::Normal).await;

    // a and b run; c stays queued. `Running` is the state-machine
    // view — poll until the ENGINES are actually active (two hops of
    // spawn + pre-flight store read sit between claim and the
    // port's counter).
    wait_for("a+b engines active", || {
        Box::pin(async { rig.port.peak_active.load(Ordering::SeqCst) == 2 })
    })
    .await;
    assert!(matches!(
        rig.sched.tasks().get(&c.id).await,
        Ok(Some(t)) if t.status == TaskStatus::Queued
    ));

    // Pause b → its engine returns Cancelled → a slot frees → c runs
    // and completes.
    rig.sched.pause(&b.id).await.unwrap();
    wait_for("c completes after slot freed", || {
        Box::pin(status_is(&rig.sched, &c.id, TaskStatus::Completed))
    })
    .await;
    // b stayed Paused (not Failed) — the Cancelled mapping contract.
    assert!(matches!(
        rig.sched.tasks().get(&b.id).await,
        Ok(Some(t)) if t.status == TaskStatus::Paused
    ));

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 3. Priority order.

#[tokio::test]
async fn higher_priority_preempts_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            // occupies the only slot
            Script::AwaitCancel {
                partial: 1,
                total: Some(10),
            },
            // low, queued first
            Script::Ok {
                bytes: 2,
                total: Some(2),
                frames: 1,
                frame_pause: None,
            },
            // high, queued second
            Script::Ok {
                bytes: 3,
                total: Some(3),
                frames: 1,
                frame_pause: None,
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let blocker = add(&rig.sched, dir.path(), "blocker", Priority::Normal).await;
    wait_for("blocker running", || {
        Box::pin(status_is(&rig.sched, &blocker.id, TaskStatus::Running))
    })
    .await;

    let low = add(&rig.sched, dir.path(), "low", Priority::Low).await;
    let high = add(&rig.sched, dir.path(), "high", Priority::High).await;
    let _ = low;
    // Let the loop observe both before freeing the slot.
    tokio::time::sleep(Duration::from_millis(50)).await;

    rig.sched.pause(&blocker.id).await.unwrap();
    // High must claim the freed slot despite Low being queued first.
    wait_for("high completes before low starts", || {
        Box::pin(status_is(&rig.sched, &high.id, TaskStatus::Completed))
    })
    .await;
    // THE assertion is ORDER: the first post-blocker job the engine
    // saw is `high`. (Low may legitimately have been picked up in
    // the same fill_slots pass that finished draining high — that is
    // the scheduler working, not a violation.)
    let jobs = rig.port.jobs();
    assert!(jobs.len() >= 2, "high must have started");
    assert_eq!(
        jobs[1].sink,
        std::path::PathBuf::from(abs_path(dir.path(), "high")),
        "second engine call must be the HIGH-priority task, got {:?}",
        jobs[1].sink
    );

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 4. Failure mapping.

#[tokio::test]
async fn engine_failure_maps_to_failed_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            // succeeds
            Script::Ok {
                bytes: 5,
                total: Some(10),
                frames: 1,
                frame_pause: None,
            },
            // fails
            Script::Fail(ApiError::Network("reset by peer".into())),
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let ok = add(&rig.sched, dir.path(), "ok", Priority::Normal).await;
    wait_for("first completes", || {
        Box::pin(status_is(&rig.sched, &ok.id, TaskStatus::Completed))
    })
    .await;

    let bad = add(&rig.sched, dir.path(), "bad", Priority::Normal).await;
    wait_for("second fails", || {
        Box::pin(status_is(&rig.sched, &bad.id, TaskStatus::Failed))
    })
    .await;
    let row = rig.sched.tasks().get(&bad.id).await.unwrap().unwrap();
    assert_eq!(row.error.as_deref(), Some("network error: reset by peer"));

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 5. Pause mid-flight: state first, engine cancelled, worker silent.

#[tokio::test]
async fn pause_mid_flight_keeps_paused_not_failed_and_lands_partial() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::AwaitCancel {
            partial: 42,
            total: Some(100),
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;

    rig.sched.pause(&t.id).await.unwrap();
    wait_for("paused", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Paused))
    })
    .await;
    // The partial reading the engine reported before hanging must
    // be in the row (finish_pending lands it). The status lands
    // BEFORE that flush (the CAS runs first), so wait for the
    // READING, not just the status — this was a 1-in-6 flake.
    wait_for("partial lands", || {
        Box::pin(async {
            matches!(
                rig.sched.tasks().get(&t.id).await,
                Ok(Some(t)) if t.received_bytes == 42 && t.total_bytes == Some(100)
            )
        })
    })
    .await;

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 6. Resume requeues and finishes.

#[tokio::test]
async fn replayed_session_with_session_base_lands_on_absolute_byte_count() {
    // R2 P2-4: the REAL engine calls `on_session_base(1000)` at
    // session entry, then (server ignored Range) returns a
    // 200-replay outcome. finish must rebase on 0 (replayed) —
    // NOT keep the 1000 base — landing on 5000, not 6000/5000.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1000,
                total: Some(100_000),
            },
            Script::OkReplayedAfterBase {
                base: 1000,
                bytes: 5000,
                total: Some(100_000),
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;
    rig.sched.pause(&t.id).await.unwrap();
    wait_for("paused", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Paused))
    })
    .await;
    std::fs::write(dir.path().join("f.bin"), vec![0u8; 1000]).unwrap();

    rig.sched.resume(&t.id).await.unwrap();
    wait_for("completed after base+replay resume", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 5000,
        "replay discards even a declared session base"
    );

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn replayed_from_zero_session_does_not_double_count_resume_offset() {
    // B34 regression: the resumed session's engine hit a 200 full
    // replay (server ignored Range / If-Range rejected) and rewrote
    // the sink from zero. The caller's resume offset (1000, the
    // on-disk partial) was DISCARDED — `finish` must rebase on 0,
    // landing received == 5000, not 1000 + 5000.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1000,
                total: Some(100_000),
            },
            Script::OkReplayed {
                bytes: 5000,
                total: Some(100_000),
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;
    rig.sched.pause(&t.id).await.unwrap();
    wait_for("paused", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Paused))
    })
    .await;
    // The on-disk partial resume_job will see (the scripted engine
    // never touches the file — write it ourselves).
    std::fs::write(dir.path().join("f.bin"), vec![0u8; 1000]).unwrap();

    rig.sched.resume(&t.id).await.unwrap();
    wait_for("completed after replayed resume", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 5000,
        "200-replay discards the resume offset — no double count"
    );

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn resumed_session_rebases_onto_row_reading() {
    // M3-b1 smoke P0 regression: a paused task's engine cursors lag
    // the row's received_bytes (the tail quantum reported but not
    // yet flushed to cursors). The resumed session declares its own
    // base (600 here) BELOW the row's landed reading (1000). Raw
    // absolute accounting would freeze the row at 1000 until the
    // session out-ran the tail; re-basing (`seed + (v - base)`)
    // advances it from the very first frame. Final: 1000 + 5000.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            // paused with 1000 bytes reported
            Script::AwaitCancel {
                partial: 1000,
                total: Some(100_000),
            },
            // resumed: cursors sum 600, session adds 5000
            Script::OkFromBase {
                base: 600,
                bytes: 5000,
                total: Some(100_000),
                frames: 5,
                frame_pause: None,
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;
    rig.sched.pause(&t.id).await.unwrap();
    wait_for("paused", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Paused))
    })
    .await;
    wait_for("paused partial lands", || {
        Box::pin(async {
            matches!(
                rig.sched.tasks().get(&t.id).await,
                Ok(Some(t)) if t.received_bytes == 1000
            )
        })
    })
    .await;

    rig.sched.resume(&t.id).await.unwrap();
    wait_for("completed after resume", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 6000,
        "session is re-based onto the row reading (1000 + 5000), not the raw max (5600)"
    );

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn resumed_completion_reading_is_clamped_to_total() {
    // GUI-verify R2 P1: the paused-tail rebase (`seed + (v - base)`)
    // puts a completed resumed session's readings ABOVE total by the
    // tail quantum (row 1000, cursors 600 → final reading 100_400 on
    // a 100_000-byte file). A drainer tick lands that overshoot under
    // the store's `MAX(received, ?)` clamp before finish()'s own
    // clamp can run, and the completed row shows `18.3 MB / 15.5 MB`
    // (118%). Flushes must clamp to the known total.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1000,
                total: Some(100_000),
            },
            Script::OkFromBase {
                base: 600,
                bytes: 99_400,
                total: Some(100_000),
                frames: 3,
                frame_pause: Some(Duration::from_millis(400)),
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;
    rig.sched.pause(&t.id).await.unwrap();
    wait_for("paused", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Paused))
    })
    .await;
    wait_for("paused partial lands", || {
        Box::pin(async {
            matches!(
                rig.sched.tasks().get(&t.id).await,
                Ok(Some(t)) if t.received_bytes == 1000
            )
        })
    })
    .await;
    std::fs::write(dir.path().join("f.bin"), vec![0u8; 600]).unwrap();

    rig.sched.resume(&t.id).await.unwrap();
    wait_for("completed after resume", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 100_000,
        "completed reading clamps to total — no paused-tail overshoot (was 100_400)"
    );

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn set_max_concurrent_hot_applies_to_queued_tasks() {
    // GUI batch-2: the settings center raises the live concurrency
    // budget — a task queued behind a full budget must START without
    // waiting for the running task to finish (the old hardcoded
    // budget could only change with a daemon restart).
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1000,
                total: Some(100_000),
            },
            Script::AwaitCancel {
                partial: 0,
                total: Some(100_000),
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t1 = add(&rig.sched, dir.path(), "a.bin", Priority::Normal).await;
    wait_for("first running", || {
        Box::pin(status_is(&rig.sched, &t1.id, TaskStatus::Running))
    })
    .await;
    let t2 = add(&rig.sched, dir.path(), "b.bin", Priority::Normal).await;
    wait_for("second queued behind full budget", || {
        Box::pin(async {
            matches!(
                rig.sched.tasks().get(&t2.id).await,
                Ok(Some(t)) if t.status == TaskStatus::Queued
            )
        })
    })
    .await;

    rig.sched.set_max_concurrent(2).await;
    wait_for("second running after hot apply", || {
        Box::pin(async {
            matches!(
                rig.sched.tasks().get(&t2.id).await,
                Ok(Some(t)) if t.status == TaskStatus::Running
            )
        })
    })
    .await;

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn readd_completed_target_reports_full_progress() {
    // GUI-verify R1 P1 (H1): re-adding a target whose old (url,
    // sink) plan is already complete resurrects that plan onto a
    // FRESH row (seed 0). The engine declares base = total (disk
    // truth) and reports completion; before the seed-lift fix,
    // rebase() mapped total onto `0 + (total - total)` = 0 and the
    // row landed completed with received_bytes = 0 — the "0 B /
    // 36.5 MB" row. After: the seed lifts to the base, rebase is
    // the identity, and the row lands received == total.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::OkFromBase {
            base: 100_000,
            bytes: 0,
            total: Some(100_000),
            frames: 1,
            frame_pause: None,
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("completed", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 100_000,
        "a resurrected complete plan must land its absolute bytes, not 0"
    );
    assert_eq!(row.total_bytes, Some(100_000));

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn readd_partial_target_reports_absolute_progress() {
    // GUI-verify R1 P1 (H2, same root): re-adding a target whose
    // old plan is PARTIALLY done (60k on disk) seeds the fresh row
    // at 0 while the engine declares base = 60_000 and adds 40_000
    // session bytes (4 frames × 10k). Before the fix, rebase()
    // reported only the session delta (40k). After: readings map
    // to absolutes (60k → 100k).
    //
    // Speed-shape guard (R2 P1-1): the 60k of already-on-disk
    // bytes materializes as ONE first publish (the seed lift),
    // never as mid-session jumps — every LATER publish may only
    // advance by whole frames (10k each; ≤2 frames can coalesce
    // into one drainer tick, hence the ×2 tolerance). A consumer
    // can treat a first-publish jump as a baseline reset instead
    // of speed; a mid-stream jump would be phantom speed.
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::OkFromBase {
            base: 60_000,
            bytes: 40_000,
            total: Some(100_000),
            frames: 4,
            // Spread frames across drainer ticks (progress_interval
            // = 250 ms) so each frame lands as its own publish.
            frame_pause: Some(Duration::from_millis(300)),
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());
    let mut rx = rig.bus.subscribe();

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("completed", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 100_000,
        "the row must land the ABSOLUTE progress (base + session), not the delta"
    );

    // Collect this task's TaskProgress publishes in order.
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let EngineEvent::TaskProgress { id, received, .. } = ev
            && id == t.id
        {
            events.push(received);
        }
    }
    assert!(
        events.len() >= 2,
        "expected progress publishes, got {events:?}"
    );
    // First publish carries the disk truth (the lift) plus at
    // most one frame of session bytes.
    assert!(
        (60_000..=70_000).contains(&events[0]),
        "first publish should be base(+≤1 frame), got {}",
        events[0]
    );
    // Every later step is whole session frames — no 60k jump
    // ever appears mid-stream (that was the phantom-speed shape).
    for w in events.windows(2) {
        assert!(
            w[1] - w[0] <= 2 * 10_000,
            "mid-stream jump of {} bytes looks like phantom speed: {events:?}",
            w[1] - w[0]
        );
    }

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn resume_requeues_the_task_and_it_completes() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            // paused
            Script::AwaitCancel {
                partial: 42,
                total: Some(100),
            },
            // resumed
            Script::Ok {
                bytes: 58,
                total: Some(100),
                frames: 2,
                frame_pause: None,
            },
        ],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;
    rig.sched.pause(&t.id).await.unwrap();
    wait_for("paused", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Paused))
    })
    .await;

    rig.sched.resume(&t.id).await.unwrap();
    wait_for("completed after resume", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;
    // The second session's sink takes max(prior cumulative 42,
    // engine-reported) — the fake reports cumulative values ending at
    // 58, so the row holds 58.
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(row.received_bytes, 58);

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 7. Remove mid-flight: the row wins, a racing engine Ok is dropped.

#[tokio::test]
async fn remove_mid_flight_discards_a_racing_ok_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::RaceOk {
            bytes: 100,
            total: Some(100),
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;

    // Remove cancels the engine; the engine then returns Ok anyway
    // (a finish racing the user's remove). The row is already gone —
    // the worker's complete() must lose and stay silent.
    rig.sched.remove(&t.id, false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(rig.sched.tasks().get(&t.id).await.unwrap().is_none());
    // No worker left behind.
    assert_eq!(rig.port.active.load(Ordering::SeqCst), 0);

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 8. Progress coalescing: flood in, paced writes out, final lands.

#[tokio::test]
async fn progress_flood_is_coalesced_and_final_reading_lands() {
    let dir = tempfile::tempdir().unwrap();
    // 30 ms interval; engine floods 500 frames in well under one tick.
    let rig = rig_cfg(
        vec![Script::Ok {
            bytes: 5000,
            total: Some(5000),
            frames: 500,
            frame_pause: None,
        }],
        SchedulerConfig {
            max_concurrent: 1,
            progress_interval: Duration::from_millis(30),
            poll_interval: Duration::from_millis(10),
        },
    );
    let mut rx = rig.bus.subscribe();
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("completed", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;

    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(
        row.received_bytes, 5000,
        "final cumulative reading must land"
    );

    let mut progress_events = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, EngineEvent::TaskProgress { .. }) {
            progress_events += 1;
        }
    }
    assert!(
        progress_events < 10,
        "500-frame flood must not produce ~500 events, got {progress_events}"
    );

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 9. Disk-derived resume context.

#[tokio::test]
async fn existing_partial_file_becomes_resume_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.bin");
    std::fs::write(&path, vec![0u8; 1234]).unwrap();

    let rig = rig(
        vec![Script::Ok {
            bytes: 66,
            total: Some(1300),
            frames: 2,
            frame_pause: None,
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("completed", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;

    let jobs = rig.port.jobs();
    assert_eq!(jobs.len(), 1);
    let resume = jobs[0].resume.as_ref().expect("resume context from disk");
    assert_eq!(resume.start_offset, 1234, "offset = existing file length");
    // Cumulative row = max(engine cumulative, offset + session bytes).
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(row.received_bytes, 1234 + 66);

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 10. Shutdown drains everything without deadlocking.

#[tokio::test]
async fn shutdown_cancels_and_drains_running_workers() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1,
                total: Some(10),
            },
            Script::AwaitCancel {
                partial: 2,
                total: Some(10),
            },
        ],
        2,
    );
    tokio::spawn(rig.sched.clone().run());

    let a = add(&rig.sched, dir.path(), "a", Priority::Normal).await;
    let b = add(&rig.sched, dir.path(), "b", Priority::Normal).await;
    wait_for("both running", || {
        Box::pin(async {
            status_is(&rig.sched, &a.id, TaskStatus::Running).await
                && status_is(&rig.sched, &b.id, TaskStatus::Running).await
        })
    })
    .await;

    // Must return (both engines resolve Cancelled, both workers exit).
    let drained = tokio::time::timeout(Duration::from_secs(3), rig.sched.shutdown()).await;
    assert!(drained.is_ok(), "shutdown must not hang");
    assert_eq!(rig.port.active.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------
// R2 final-report regressions.

/// F9 rewrite: paced frames so drainer ticks deterministically
/// interleave with the flood — the old version (500 yield-only
/// frames) finished before a single tick and asserted nothing about
/// interval-paced merging.
#[tokio::test]
async fn coalescing_ticks_merge_paced_flood() {
    let dir = tempfile::tempdir().unwrap();
    // 150 frames × 2 ms ≈ 300 ms flood, 30 ms interval → expect
    // ~10 merged events (>=2 proves real mid-flood merging, <25
    // proves the interval ceiling held).
    let rig = rig_cfg(
        vec![Script::Ok {
            bytes: 15_000,
            total: Some(15_000),
            frames: 150,
            frame_pause: Some(Duration::from_millis(2)),
        }],
        SchedulerConfig {
            max_concurrent: 1,
            progress_interval: Duration::from_millis(30),
            poll_interval: Duration::from_millis(10),
        },
    );
    let mut rx = rig.bus.subscribe();
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "f.bin", Priority::Normal).await;
    wait_for("completed", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Completed))
    })
    .await;

    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(row.received_bytes, 15_000, "final reading must land");

    let mut progress_events = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, EngineEvent::TaskProgress { .. }) {
            progress_events += 1;
        }
    }
    assert!(
        (2..25).contains(&progress_events),
        "paced 150-frame flood should merge to ~10 events, got {progress_events}"
    );

    let drained = rig.sched.shutdown().await;
    assert_eq!(drained, 0);
}

/// F1: the most common user action — double-adding the same link.
/// The second add must be rejected while the first is active, and
/// allowed again once the row is terminal.
#[tokio::test]
async fn duplicate_active_target_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1,
                total: Some(10),
            },
            Script::Ok {
                bytes: 10,
                total: Some(10),
                frames: 1,
                frame_pause: None,
            },
        ],
        2,
    );
    tokio::spawn(rig.sched.clone().run());

    let target = abs_path(dir.path(), "dup.bin");
    let a = rig
        .sched
        .add("http://test/file", target.clone(), Priority::Normal)
        .await
        .unwrap();
    wait_for("a running", || {
        Box::pin(status_is(&rig.sched, &a.id, TaskStatus::Running))
    })
    .await;

    // Same (url, save_path) while active → rejected.
    match rig
        .sched
        .add("http://test/file", target.clone(), Priority::Normal)
        .await
    {
        Err(TaskError::DuplicateActive { .. }) => {}
        other => panic!("expected DuplicateActive, got {other:?}"),
    }

    // Remove → terminal → the same target is addable again.
    rig.sched.remove(&a.id, false).await.unwrap();
    // Remove is idempotent (F11): a second remove reports success.
    rig.sched.remove(&a.id, false).await.unwrap();
    let b = rig
        .sched
        .add("http://test/file", target, Priority::Normal)
        .await
        .unwrap();
    wait_for("b completes", || {
        Box::pin(status_is(&rig.sched, &b.id, TaskStatus::Completed))
    })
    .await;

    rig.sched.shutdown().await;
}

/// F1 (path mutex): a DIFFERENT url to the SAME file must not run
/// concurrently with the first — the claim loop skips it while the
/// path is busy and picks it up after the path frees.
#[tokio::test]
async fn same_save_path_tasks_serialize() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![
            Script::AwaitCancel {
                partial: 1,
                total: Some(10),
            },
            Script::Ok {
                bytes: 5,
                total: Some(5),
                frames: 1,
                frame_pause: None,
            },
        ],
        2, // budget 2 — only the path blocks them
    );
    tokio::spawn(rig.sched.clone().run());

    let target = abs_path(dir.path(), "same.bin");
    let a = rig
        .sched
        .add("http://one/file", target.clone(), Priority::Normal)
        .await
        .unwrap();
    wait_for("a running", || {
        Box::pin(status_is(&rig.sched, &a.id, TaskStatus::Running))
    })
    .await;

    let b = rig
        .sched
        .add("http://two/file", target, Priority::Normal)
        .await
        .unwrap();
    // Give the loop every chance to (wrongly) start b.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        rig.port.peak_active.load(Ordering::SeqCst),
        1,
        "two engines must never write one file concurrently"
    );
    assert_eq!(b.status, TaskStatus::Queued);

    // Free the path; b must now run and complete.
    rig.sched.pause(&a.id).await.unwrap();
    rig.sched.remove(&a.id, false).await.unwrap();
    wait_for("b completes after path freed", || {
        Box::pin(status_is(&rig.sched, &b.id, TaskStatus::Completed))
    })
    .await;

    rig.sched.shutdown().await;
}

/// F8/F10-C: a panicking port must not wedge anything — the row goes
/// to Failed with a diagnostic, the slot frees, later tasks run.
#[tokio::test]
async fn panicking_port_fails_task_and_frees_slot() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::Panic, Script::Fail(ApiError::Io("after".into()))],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let a = add(&rig.sched, dir.path(), "a.bin", Priority::Normal).await;
    wait_for("a failed via panic supervisor", || {
        Box::pin(status_is(&rig.sched, &a.id, TaskStatus::Failed))
    })
    .await;
    let row = rig.sched.tasks().get(&a.id).await.unwrap().unwrap();
    assert!(
        row.error
            .as_deref()
            .unwrap_or_default()
            .contains("panicked"),
        "panic must land a diagnostic, got {:?}",
        row.error
    );

    // The slot freed: a second task runs immediately (would starve
    // forever if the panic leaked the slot).
    let b = add(&rig.sched, dir.path(), "b.bin", Priority::Normal).await;
    wait_for("b failed (second script)", || {
        Box::pin(status_is(&rig.sched, &b.id, TaskStatus::Failed))
    })
    .await;

    let drained = rig.sched.shutdown().await;
    assert_eq!(drained, 0);
}

/// F3: shutdown must not churn never-started tasks to Running.
#[tokio::test]
async fn shutdown_leaves_unstarted_tasks_queued() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::AwaitCancel {
            partial: 1,
            total: Some(10),
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let a = add(&rig.sched, dir.path(), "a.bin", Priority::Normal).await;
    wait_for("a running", || {
        Box::pin(status_is(&rig.sched, &a.id, TaskStatus::Running))
    })
    .await;
    // b sits queued behind the budget; shutdown fires mid-queue.
    let b = add(&rig.sched, dir.path(), "b.bin", Priority::Normal).await;

    let drained = rig.sched.shutdown().await;
    assert_eq!(drained, 0);
    let row = rig.sched.tasks().get(&b.id).await.unwrap().unwrap();
    assert_eq!(
        row.status,
        TaskStatus::Queued,
        "never-started task must stay Queued across shutdown, got {:?}",
        row.status
    );
}

// ---- M3-b: rate limits -------------------------------------------------

#[tokio::test]
async fn set_task_limit_persists_and_routes_to_port() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(
        vec![Script::AwaitCancel {
            partial: 1,
            total: Some(10),
        }],
        1,
    );
    tokio::spawn(rig.sched.clone().run());

    let t = add(&rig.sched, dir.path(), "lim.bin", Priority::Normal).await;
    wait_for("running", || {
        Box::pin(status_is(&rig.sched, &t.id, TaskStatus::Running))
    })
    .await;

    // 256 KiB/s
    let updated = rig.sched.set_task_limit(&t.id, 262_144).await.unwrap();
    assert_eq!(updated.speed_limit_bps, 262_144);

    // Row persisted...
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(row.speed_limit_bps, 262_144);
    // ...and the RUNNING engine was poked live (url + sink, bps).
    let calls = rig.port.limit_calls();
    assert_eq!(calls.len(), 1, "exactly one live poke, got {calls:?}");
    assert_eq!(calls[0].0, t.url);
    assert_eq!(calls[0].1, std::path::PathBuf::from(&t.save_path));
    assert_eq!(calls[0].2, Some(262_144));

    // Unlimited (0) pokes with None.
    rig.sched.set_task_limit(&t.id, 0).await.unwrap();
    let calls = rig.port.limit_calls();
    assert_eq!(calls[1].2, None);

    // Unknown id → NotFound, no poke.
    let err = rig
        .sched
        .set_task_limit(&peregrine_api::TaskId::new("nope"), 1)
        .await
        .unwrap_err();
    assert!(matches!(err, TaskError::NotFound(_)), "got {err:?}");
    assert_eq!(rig.port.limit_calls().len(), 2);

    rig.sched.shutdown().await;
}

#[tokio::test]
async fn global_limit_persists_and_restores() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("g.db");
    // Two daemons over the SAME db file: set, then boot fresh.
    let bus = EventBus::new(1024);
    let tm = Arc::new(TaskManager::new(
        peregrine_storage::Store::open(&db).unwrap(),
        bus.clone(),
    ));
    let port = ScriptedPort::new(vec![]);
    let global = peregrine_api::budget::RateBudget::unlimited();
    let sched = Arc::new(Scheduler::new(
        tm,
        bus.clone(),
        port,
        global.clone(),
        SchedulerConfig::default(),
    ));

    assert_eq!(global.bps(), 0, "fresh daemon is unlimited");
    sched.set_global_limit(524_288).await.unwrap();
    assert_eq!(global.bps(), 524_288, "applied live");

    // Fresh daemon, same db: restore must re-apply the persisted value.
    let bus2 = EventBus::new(1024);
    let tm2 = Arc::new(TaskManager::new(
        peregrine_storage::Store::open(&db).unwrap(),
        bus2.clone(),
    ));
    let global2 = peregrine_api::budget::RateBudget::unlimited();
    let sched2 = Arc::new(Scheduler::new(
        tm2,
        bus2,
        ScriptedPort::new(vec![]),
        global2.clone(),
        SchedulerConfig::default(),
    ));
    let restored = sched2.restore_global_limit().await.unwrap();
    assert_eq!(restored, 524_288);
    assert_eq!(global2.bps(), 524_288, "restored into the live budget");
}
