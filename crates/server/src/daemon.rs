//! Daemon composition root (M2-d, PROPOSAL §3.2 `server/`): the ONE
//! place where the crates are wired into a working process.
//!
//! Assembly rules this module owns (each is a real footgun the R2
//! reviews caught as latent bugs — see BACKLOG B31/B32):
//! 1. ONE `Store` instance, cloned into every consumer. Two
//!    `Store::open` calls on the same file have no busy_timeout and
//!    deadlock SQLITE_BUSY each other (B32).
//! 2. `Scheduler` is created but `run()` is spawned by the CALLER —
//!    `build()` stays sync-ish and testable; the binary (or a test)
//!    decides task lifetime. Tests want the loop running before the
//!    first assertion; main wants it under its shutdown umbrella.
//! 3. Crash recovery (`tm.boot()`) runs BEFORE the scheduler loop —
//!    a stale `Running` row must be re-queued before anything can
//!    claim it.
//!
//! What lives here on purpose: paths + defaults (single source with
//! `peregrine_api::transport`), and the choice that the production
//! port is `HttpAutoPort` over `HttpEngine::download_auto`. What
//! stays out: policy (SchedulerConfig defaults are the scheduler's),
//! HTTP details (engine crate), persistence shape (storage crate).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use peregrine_api::TaskStatus;
use peregrine_api::bus::EventBus;
use peregrine_engine_http::{HttpEngine, SegmentConfig};
use peregrine_scheduler::{DownloadPort, HttpAutoPort, Scheduler, SchedulerConfig};
use peregrine_storage::Store;
use peregrine_task_manager::TaskManager;

/// The wired daemon: everything the REST/WS surface needs, nothing it
/// doesn't. `sched` is the facade clients drive; `bus` feeds `/events`.
pub struct Daemon {
    pub sched: Arc<Scheduler>,
    pub bus: EventBus,
    /// The db file we opened (diagnostics, tests).
    pub db_path: PathBuf,
    /// Double-boot guard: `start()` twice is a wiring bug, not a
    /// retried RPC — make it loud instead of two loops racing the
    /// claim CAS (one always losing, burning a wake cycle per claim).
    booted: std::sync::atomic::AtomicBool,
    /// `/health` uptime anchor (M0 identity: pid + uptime).
    started: std::time::Instant,
}

impl Clone for Daemon {
    /// Shares the SAME scheduler/bus/store (all Arc) plus a snapshot
    /// of the boot flag — the flag only guards the one-shot
    /// `start()`, and every clone made from a booted daemon reports
    /// booted, so a re-start through any handle still fails loudly.
    fn clone(&self) -> Self {
        Self {
            sched: Arc::clone(&self.sched),
            bus: self.bus.clone(),
            db_path: self.db_path.clone(),
            booted: std::sync::atomic::AtomicBool::new(
                self.booted.load(std::sync::atomic::Ordering::SeqCst),
            ),
            started: self.started,
        }
    }
}

impl Daemon {
    /// Build a production daemon over a real HTTP engine.
    ///
    /// `db_path: None` → `default_db_path()` (XDG data dir). Callers
    /// (tests) pass an explicit temp path.
    pub fn build(
        db_path: Option<&Path>,
        sched_cfg: SchedulerConfig,
        seg_cfg: SegmentConfig,
    ) -> anyhow::Result<Self> {
        Self::assemble(db_path, sched_cfg, |store| {
            let engine = Arc::new(HttpEngine::new()?);
            Ok(Arc::new(HttpAutoPort::new(engine, seg_cfg, store)))
        })
    }

    /// Assembly entry for tests: inject a scripted `DownloadPort` and
    /// drive the full daemon stack (scheduler + bus + REST) without
    /// HTTP. The single-Store rule holds — the port never opens its
    /// own store; the scheduler's TaskManager and the port SHARE one.
    pub fn build_with_port(
        db_path: Option<&Path>,
        sched_cfg: SchedulerConfig,
        port: Arc<dyn DownloadPort>,
    ) -> anyhow::Result<Self> {
        Self::assemble(db_path, sched_cfg, |_| Ok(port))
    }

    /// The one assembly path: exactly one `Store` is opened, the
    /// factory turns it into the port, everything else is clone-out.
    fn assemble(
        db_path: Option<&Path>,
        sched_cfg: SchedulerConfig,
        port_factory: impl FnOnce(Store) -> anyhow::Result<Arc<dyn DownloadPort>>,
    ) -> anyhow::Result<Self> {
        let path = match db_path {
            Some(p) => p.to_path_buf(),
            None => peregrine_api::default_db_path()?,
        };
        // RULE: single Store, cloned everywhere (B32).
        let store = Store::open(&path)?;
        let bus = EventBus::default();
        let tm = Arc::new(TaskManager::new(store.clone(), bus.clone()));
        let port = port_factory(store)?;
        let sched = Arc::new(Scheduler::new(tm, bus.clone(), port, sched_cfg));
        Ok(Self {
            sched,
            bus,
            db_path: path,
            booted: AtomicBool::new(false),
            started: std::time::Instant::now(),
        })
    }

    /// The COMPLETE daemon router (REST + WS + /health). This is
    /// the one place the wire surface is assembled — `main.rs` and
    /// every test drive the same router, so the tested surface IS
    /// the shipped surface (no merge drift between them).
    pub fn router(self: &Arc<Self>) -> axum::Router {
        crate::api::router(crate::api::AppState(self.clone())).merge(crate::health::router(
            crate::health::Health {
                pid: std::process::id(),
                started: self.started,
            },
        ))
    }

    /// Crash recovery + scheduler loop. Idempotent guard: booting
    /// twice would re-queue Running rows that the live loop already
    /// owns; the flag makes that a hard error instead of silent
    /// double-claims racing the CAS.
    ///
    /// Returns the number of interrupted tasks re-queued.
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<usize> {
        if self.booted.swap(true, Ordering::SeqCst) {
            anyhow::bail!("daemon already started");
        }
        let requeued = self.sched.tasks().boot().await?;
        let daemon = Arc::clone(self);
        tokio::spawn(async move {
            let sched = Arc::clone(&daemon.sched);
            let handle = tokio::spawn({
                let sched = Arc::clone(&sched);
                async move { sched.run().await }
            });
            // Run-loop supervision (R2 P1-2): `run()` returning or
            // panicking means NO TASK WILL EVER START AGAIN while
            // REST keeps answering 200s — the daemon's own
            // invariant doc calls that crash-worthy. Fail every
            // running row loudly, then exit the process: a
            // supervisor (or the user) restarts into crash recovery,
            // which is the designed net for exactly this state.
            match handle.await {
                Ok(()) => {}
                Err(join_err) => {
                    tracing::error!(error = %join_err, "scheduler run loop died");
                    let rows = sched.tasks().list(None).await.unwrap_or_default();
                    for t in rows.into_iter().filter(|t| t.status == TaskStatus::Running) {
                        let _ = sched
                            .tasks()
                            .fail(&t.id, "internal error: scheduler run loop died")
                            .await;
                    }
                    std::process::exit(1);
                }
            }
        });
        Ok(requeued)
    }

    /// Access the task-manager facade directly (list/get — the read
    /// path the REST layer uses without going through scheduler
    /// policy methods).
    pub fn tasks(&self) -> &TaskManager {
        self.sched.tasks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "peregrine-daemon-test-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("tasks.db")
    }

    #[tokio::test]
    async fn builds_with_single_store_and_boots_clean() {
        let db = tmpdb("build");
        let daemon = Daemon::build(
            Some(&db),
            SchedulerConfig::default(),
            SegmentConfig::default(),
        )
        .expect("build");
        let daemon = Arc::new(daemon);
        let requeued = daemon.start().await.expect("start");
        assert_eq!(requeued, 0, "fresh db has nothing to requeue");
        // Read path works through the composition root.
        assert!(daemon.tasks().list(None).await.unwrap().is_empty());
        daemon.sched.shutdown().await;
    }

    #[tokio::test]
    async fn second_start_is_rejected_not_racy() {
        let db = tmpdb("double");
        let daemon = Arc::new(
            Daemon::build(
                Some(&db),
                SchedulerConfig::default(),
                SegmentConfig::default(),
            )
            .unwrap(),
        );
        daemon.start().await.expect("first start");
        assert!(
            daemon.start().await.is_err(),
            "second start must be a hard error (double boot guard)"
        );
        daemon.sched.shutdown().await;
    }
}
