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
use peregrine_task_manager::TaskManager;
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
    /// Fail immediately.
    Fail(ApiError),
}

struct ScriptedPort {
    scripts: Mutex<VecDeque<Script>>,
    jobs: Mutex<Vec<DownloadJob>>,
    active: AtomicUsize,
    peak_active: AtomicUsize,
}

impl ScriptedPort {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            jobs: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            peak_active: AtomicUsize::new(0),
        })
    }

    fn jobs(&self) -> Vec<DownloadJob> {
        self.jobs.lock().unwrap().clone()
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
}

fn outcome(bytes: u64, total: Option<u64>) -> DownloadOutcome {
    DownloadOutcome {
        bytes_written: bytes,
        total_bytes: total,
        completed: true,
        final_url: "http://test/file".into(),
        final_validator: None,
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
        Script::Fail(e) => Err(e),
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
    let sched = Arc::new(Scheduler::new(tm, bus.clone(), port.clone(), cfg));
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

    let a = add(&rig.sched, dir.path(), "a", Priority::Normal).await;
    let b = add(&rig.sched, dir.path(), "b", Priority::Normal).await;
    let c = add(&rig.sched, dir.path(), "c", Priority::Normal).await;

    // a and b run; c stays queued.
    wait_for("a+b running", || {
        Box::pin(async {
            status_is(&rig.sched, &a.id, TaskStatus::Running).await
                && status_is(&rig.sched, &b.id, TaskStatus::Running).await
        })
    })
    .await;
    assert_eq!(rig.port.peak_active.load(Ordering::SeqCst), 2);
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
    // The partial reading the engine reported before hanging must be
    // in the row (finish_pending lands it).
    let row = rig.sched.tasks().get(&t.id).await.unwrap().unwrap();
    assert_eq!(row.received_bytes, 42);
    assert_eq!(row.total_bytes, Some(100));

    rig.sched.shutdown().await;
}

// ---------------------------------------------------------------------
// 6. Resume requeues and finishes.

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
    rig.sched.remove(&t.id).await.unwrap();
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
