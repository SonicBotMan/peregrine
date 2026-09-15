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
use peregrine_scheduler::{DownloadPort, HlsAutoPort, HttpAutoPort, Scheduler, SchedulerConfig};
use peregrine_storage::Store;
use peregrine_task_manager::TaskManager;

/// URL-routed port (M4-a): `.m3u8` targets go to the HLS merge
/// engine, everything else to the HTTP auto port. Same trait, so
/// the scheduler is oblivious — protocol selection is an assembly
/// concern, exactly like the PROPOSAL's "protocol = trait" rule.
struct RoutingPort {
    hls: Arc<dyn DownloadPort>,
    ftp: Arc<dyn DownloadPort>,
    bt: Arc<dyn DownloadPort>,
    http: Arc<dyn DownloadPort>,
}

impl RoutingPort {
    fn route(&self, url: &str) -> &Arc<dyn DownloadPort> {
        // Single definition (R2 P2-1): the engine owns the heuristic;
        // routing and supports() can never diverge. Scheme is
        // AUTHORITATIVE and judged first — the HLS heuristic below
        // only looks for "m3u8" anywhere in the string and would
        // otherwise swallow ftp://h/playlist.m3u8 (M4-c R2 P2-1).
        if url::Url::parse(url)
            .map(|u| u.scheme() == "ftp")
            .unwrap_or(false)
        {
            &self.ftp
        } else if peregrine_engine_bt::is_bt_source(url) {
            &self.bt
        } else if peregrine_engine_hls::is_hls_url(url) {
            &self.hls
        } else {
            &self.http
        }
    }
}

impl DownloadPort for RoutingPort {
    fn auto_download(
        &self,
        job: peregrine_api::DownloadJob,
        progress: peregrine_api::SharedProgressSink,
        cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<peregrine_api::DownloadOutcome, peregrine_api::ApiError>,
                > + Send
                + '_,
        >,
    > {
        self.route(&job.url).auto_download(job, progress, cancel)
    }

    fn purge(
        &self,
        url: &str,
        sink: &std::path::Path,
        purge_files: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>> {
        // BT is DIRECTED, not fanned out: its purge deletes the sink
        // path itself (torrent data), which would nuke a live
        // HTTP/HLS target if sprayed blindly. A BT source routes
        // deterministically, so route-then-purge is correct here.
        if peregrine_engine_bt::is_bt_source(url) {
            return self.bt.purge(url, sink, purge_files);
        }
        // Purge ALL sides: a URL that routes to HLS today may have
        // HTTP engine rows from a pre-M4 attempt (or vice versa after
        // a heuristic flip). Purging is idempotent — no downside.
        let a = self.http.purge(url, sink, purge_files);
        let b = self.hls.purge(url, sink, purge_files);
        let c = self.ftp.purge(url, sink, purge_files);
        Box::pin(async move {
            // ALL sides must run even if one fails (R2 P2-4):
            // short-circuiting the rest leaves stale engine state.
            let (ra, rb, rc) = tokio::join!(a, b, c);
            let errs: Vec<anyhow::Error> =
                [ra, rb, rc].into_iter().filter_map(|r| r.err()).collect();
            match errs.len() {
                0 => Ok(()),
                1 => Err(errs.into_iter().next().unwrap()),
                _ => Err(anyhow::anyhow!(
                    "purge failed on {} engines: {}",
                    errs.len(),
                    errs.iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
            }
        })
    }

    fn set_task_limit(&self, url: &str, sink: &std::path::Path, bps: Option<u64>) {
        if peregrine_engine_bt::is_bt_source(url) {
            // Loud, not silent (R2 F4): pretending to throttle a BT
            // task would violate the REST contract every other
            // engine honors. Runtime BT limits need librqbit's
            // limits API — tracked in BACKLOG.
            tracing::warn!(url, bps = ?bps, "BT rate limit not supported yet (BACKLOG); ignoring");
            return;
        }
        self.http.set_task_limit(url, sink, bps);
        self.hls.set_task_limit(url, sink, bps);
        self.ftp.set_task_limit(url, sink, bps);
    }
}

/// The wired daemon: everything the REST/WS surface needs, nothing it
/// doesn't. `sched` is the facade clients drive; `bus` feeds `/events`.
pub struct Daemon {
    pub sched: Arc<Scheduler>,
    pub bus: EventBus,
    /// Process-wide shutdown signal: fires when SIGINT/SIGTERM begins
    /// axum's graceful drain. WS streams listen so resident event
    /// connections don't park `systemctl stop` until SIGKILL (M6-c R2).
    pub cancel: tokio_util::sync::CancellationToken,
    /// Daemon-wide download rate limit (M3-b). 0 = unlimited. Every
    /// engine consults it through its `BudgetChain`; the settings
    /// endpoint pokes it live via `set_bps`.
    pub global_budget: peregrine_api::budget::SharedRateBudget,
    /// The single store (B32): kept for telemetry reads (segment
    /// cursors) that bypass the task-manager's lifecycle surface.
    pub store: peregrine_storage::Store,
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
            cancel: self.cancel.clone(),
            store: self.store.clone(),
            global_budget: self.global_budget.clone(),
            db_path: self.db_path.clone(),
            booted: std::sync::atomic::AtomicBool::new(
                self.booted.load(std::sync::atomic::Ordering::SeqCst),
            ),
            started: self.started,
        }
    }
}

/// `<db-dir>/bt-registry.json` — same directory as the task DB.
fn store_path_with_suffix(db: &Path, suffix: &str) -> PathBuf {
    let file = db.file_name().map(|f| f.to_string_lossy().into_owned());
    let name = match file {
        Some(f) => {
            let stem = f.split('.').next().unwrap_or("peregrine");
            format!("{stem}-{suffix}")
        }
        None => suffix.to_string(),
    };
    match db.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
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
        Self::assemble(db_path, sched_cfg, |store, global, path| {
            // Same shared store as HttpAutoPort (B32: ONE Store
            // instance everywhere) — wired into the engine so the
            // single-stream first response persists its validator
            // (B36 resume If-Range).
            let engine = Arc::new(HttpEngine::new()?.with_validator_store(store.clone()));
            let http: Arc<dyn DownloadPort> = Arc::new(HttpAutoPort::new(
                engine,
                seg_cfg,
                store.clone(),
                global.clone(),
            ));
            let hls: Arc<dyn DownloadPort> =
                Arc::new(HlsAutoPort::new(global.clone(), store.clone())?);
            let ftp: Arc<dyn DownloadPort> = Arc::new(peregrine_scheduler::FtpAutoPort::new(
                global.clone(),
                store.clone(),
            ));
            let bt: Arc<dyn DownloadPort> =
                Arc::new(peregrine_scheduler::BtAutoPort::with_registry_persistence(
                    // B59: the BT side table lives beside the task
                    // DB (same data dir, same lifetime).
                    // B59: the BT side table lives beside the task
                    // DB (same data dir, same lifetime).
                    store_path_with_suffix(path, "bt-registry.json"),
                ));
            Ok(Arc::new(RoutingPort { http, hls, ftp, bt }))
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
        Self::assemble(db_path, sched_cfg, |_, _, _| Ok(port))
    }

    /// The one assembly path: exactly one `Store` is opened, the
    /// factory turns it into the port, everything else is clone-out.
    fn assemble(
        db_path: Option<&Path>,
        sched_cfg: SchedulerConfig,
        port_factory: impl FnOnce(
            Store,
            peregrine_api::budget::SharedRateBudget,
            &Path,
        ) -> anyhow::Result<Arc<dyn DownloadPort>>,
    ) -> anyhow::Result<Self> {
        let path = match db_path {
            Some(p) => p.to_path_buf(),
            None => peregrine_api::default_db_path()?,
        };
        // RULE: single Store, cloned everywhere (B32).
        let store = Store::open(&path)?;
        let bus = EventBus::default();
        let tm = Arc::new(TaskManager::new(store.clone(), bus.clone()));
        let global_budget = peregrine_api::budget::RateBudget::unlimited();
        let port = port_factory(store.clone(), global_budget.clone(), &path)?;
        let sched = Arc::new(Scheduler::new(
            tm,
            bus.clone(),
            port,
            global_budget.clone(),
            sched_cfg,
        ));
        Ok(Self {
            sched,
            bus,
            cancel: tokio_util::sync::CancellationToken::new(),
            store,
            global_budget,
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
        // B24 orphan GC, BEFORE boot requeues anything: engine rows
        // whose sink file is gone can never resume — drop them now so
        // a requeued task against a deleted file starts FRESH instead
        // of gluing onto rows that describe a nonexistent disk state.
        // Best-effort: a GC failure logs and continues (stale rows are
        // a cost, not a correctness risk).
        match self.store.purge_missing_sinks().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                rows = n,
                "startup GC dropped engine rows with missing sinks"
            ),
            Err(e) => tracing::warn!(error = %e, "startup orphan GC failed"),
        }
        let requeued = self.sched.tasks().boot().await?;
        // M3-b: the persisted global rate limit applies from the
        // first byte of the first task after boot (missing = 0).
        let restored = self.sched.restore_global_limit().await?;
        if restored > 0 {
            tracing::info!(bps = restored, "restored global rate limit");
        }
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

    /// Telemetry read (M3-c1): the task's planned segment rows as
    /// wire views. `Ok(None)` = no such task (404); `Ok(vec![])` =
    /// task exists, single-stream (no plan rows). Reads the store
    /// DIRECTLY — segment cursors are engine bookkeeping, not task
    /// lifecycle state, so the task-manager (whose every method is
    /// a state-machine transition or a task-shaped read) is the
    /// wrong surface for them.
    /// Telemetry read (M3-c1): the task's planned segment rows as
    /// wire views. `Ok(None)` = no such task (404); `Ok(vec![])` =
    /// task exists, single-stream (no plan rows). Resolves the wire
    /// id through the task row (url, sink) into the ENGINE table's
    /// row id — two id spaces by design: the wire id is a string the
    /// client holds; the engine tables key on their own i64 rows
    /// (v1 rule: storage ids never leak past the engine).
    pub async fn segments_of(
        &self,
        id: &peregrine_api::TaskId,
    ) -> Result<Option<Vec<peregrine_api::SegmentView>>, peregrine_task_manager::TaskError> {
        let Some(task) = self.tasks().get(id).await? else {
            return Ok(None);
        };
        // No engine row = never segmented (single-stream, or the
        // engine hasn't planned yet): the task EXISTS, so the panel
        // shows "single stream", not an error. `[]`, never 404.
        Ok(Some(
            self.store
                .get_task(&task.url, std::path::Path::new(&task.save_path))
                .await
                .map_err(peregrine_task_manager::TaskError::Storage)?
                .map(|t| {
                    t.segments
                        .into_iter()
                        .map(|s| peregrine_api::SegmentView {
                            idx: s.idx,
                            start: s.start,
                            end: s.end,
                            len: s.len(),
                            done: s.done,
                            frontier: s.frontier(),
                            pct: if s.is_empty() {
                                1.0
                            } else {
                                s.done as f64 / s.len() as f64
                            },
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ))
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
