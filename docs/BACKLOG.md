# Backlog & Deferred Decisions

Findings reviewed and consciously deferred, with reasons. Revisit at the
milestone noted. (R1 review, 2026-02-27)

| # | Item | Deferred to | Reason |
| --- | ------ | ------------- | -------- |
| B1 | `EngineEvent::TaskProgress` coalescing (high-frequency events flood the 1024-slot bus) | M2 | No producers yet; PROPOSAL §5 already mandates coalescing when events are wired |
| B2 | Config file format decision (KDL vs RON vs TOML) | M5 | No config surface in M0; deciding now would be speculation |
| B3 | `rust-version` pin in Cargo.toml (vs rust-toolchain.toml) | M6 (CI round) | rust-toolchain.toml already locks 1.96.1 locally; CI picks it up |
| B4 | Long aliases for version flags (`-V`/`--version` on subcommands) | M6 | Zero-value polish until the command surface grows |
| B5 | `pg --json` flag for machine-readable output | M2 | MCP bridge is the real machine interface; premature now |
| B6 | Event bus capacity benchmarking (1024 slots) | M2 | Benchmarks need real event production rates |
| B7 | ~~`ProbeInfo` lacks real-Range validation~~ **DONE (M1-a, R1+R2 reviewed)**: probe now issues `Range: bytes=0-0` and decides `accept_ranges` by observed 206/200, with `Content-Range` total preferred over HEAD `Content-Length`; covered by probe.rs `probe_downgrades_servers_that_ignore_range` / `probe_upgrades_servers_that_honor_range_without_advertising` / `probe_trusts_content_range_over_head_length` | — | Completed in M1-a engine-http round |
| B8 | https→http cross-scheme redirect downgrade policy | M1 (auth headers) | Harmless without credentials; re-examine when probe/download grows auth headers (R2 self-review) |
| B9 | `ApiError::DuplicateEngine` on the IPC surface (it is a wiring bug, not a client-visible condition) | M2 (event stream) | Harmless today; revisit when events start carrying ApiError to clients (R2 self-review) |
| B10 | `serve` error path leaves the socket file; graceful shutdown has no overall deadline (a slow in-flight request can hang shutdown forever) — wrap `axum::serve` in `tokio::time::timeout(10s, ..)` and clean up on error too | M1 | Stale takeover self-heals on next start; deadline matters once real traffic exists (M2 reviewer P2-6) |
| B11 | libc double-version in graph: workspace pins `libc = "1.0.0-alpha.4"` (alpha pre-release) while tokio transitively uses `libc 0.2.189` — stopgap for a broken local mirror index | M1 | GitHub CI has Cargo.lock so builds reproduce; revert to 0.2 line when the local mirror recovers (M2 reviewer P2-8) |
| B12 | `ApiError::Io` keeps only a string; `io::ErrorKind` is lost, callers cannot branch on NotFound/PermissionDenied | M2 (with B9) | Structural change belongs with the IPC error surface, not before it (M2 reviewer P2-9) |
| B13 | Socket path hardening batch: XDG_RUNTIME_DIR owner-uid/0700 validation; /tmp fallback symlink rejection + uid ownership; residual uds.rs TOCTOU (stat→unlink handover windows, could rename-to-private-then-unlink); bind→chmod 0700 window for explicit `--socket` paths (umask tightening) | M1 hardening or M2 | User-mode today; all are defense-in-depth against anomalous environments (consolidates M2 reviewer P2-1/2/3 + P2-11) |
| B14 | Supply-chain gate: add cargo-deny (or cargo-audit) to CI — advisories + licenses + the B11 libc anomaly would surface automatically | M1 CI round (follow-up PR) | Explicitly recommended by M0 R2 review once dependencies started arriving |
| B15 | Content-Disposition parser: semicolons inside quoted filenames (currently dropped, not mis-parsed — test fixes the behavior) and `filename*` RFC 5987 encoding | M1-b/M2 | No consumer of the filename yet; parser hardening lands with real-world URL testing |
| B16 | HEAD-refusing servers (405/501): promote the ranged GET to primary probe (aria2 route) instead of failing | M1-c | Download path landed in M1-b (`download.rs`); fold the ranged-GET-primary fallback into the M1-c planner work where it will actually be exercised |
| B17 | Enable HTTP/2 ALPN in hyper-rustls for CDN-hosted large files | M2+ | HTTP/1.1 negotiates everywhere today; measure before enabling |
| B18 | Test: `uds_client` 5s timeout covering the response body (mock daemon that stalls mid-body) | M1-c | cli has no tests dir yet; add together with the first real RPC the client carries (download RPC lands with M1-c's task wiring) |
| B19 | ~~M1-b leftovers (R1 self-review): `write_mode` string comparison; bare ENOENT for missing resume target~~ **DONE in M1-c1**: `WriteMode` enum + friendly "resume target missing" error | — | Resolved by the M1-c1 refactor |
| B20 | ~~`parse_content_range` ignores the END value~~ **DONE in M1-c1**: returns `(start, end, Option<total>)`; segment workers enforce two-ended 206 match (`bytes S-E` must echo exactly), single-stream validates `end+1==total` with a warn | — | Resolved by the M1-c1 planner |
| B21 | Download redirect handling: 3xx with a Location lacking `..` normalization / relative resolution edge cases, and non-GET 304/305 semantics, are not specially covered beyond probe's shared logic | M1-c | Probe and download share the chase loop; only divergence-worthy when M1-c adds per-segment redirect budgets (M1-b R2 P3-7) |
| B22 | Concurrent `run_segmented_download` on the same (url, sink) from two callers would interleave `replace_segments` plans and double-write ranges — safe today (no concurrent callers exist), needs a task-level claim/mutex when M2's queue exposes it | M2 | Single-caller CLI today; queue layer owns task lifecycle |
| B23 | Segmented resume trusts the on-disk prefix implicitly (crash window: write-before-cursor is idempotent re-overwrite, verified); no guard against an EXTERNALLY replaced/truncated sink file between sessions — `set_len` masks it. Optional: hash or length spot-check of completed segments on resume | M2 | Defense-in-depth; single-stream path has the on_disk==start_offset check, segmented lacks the equivalent |
| B24 | Orphaned segment rows keyed by post-redirect final URL: a re-call with the original URL re-pays the probe; if the redirect target changes, the old row is orphaned forever (no TTL/GC) — add row GC at server startup or task-queue level | M2 | Correctness preserved (worker-level staleness guards); cost + cruft only (M1-c2 R2 P2-3) |
| B25 | Progress regresses across a downgrade (segmented climb → restart from 0); add monotonic clamp or a Downgrade event so UIs can reset knowingly | M2 (UI) | Cosmetic; Recorder tests don't assert monotonicity (M1-c2 R2 P2-4) |
| B26 | No-validator TOCTOU: with neither probe nor stored validator, concurrent segment workers can glue mixed generations undetectably — doc note in design/PROPOSAL §5, plus optional first-byte hash spot-check | M2 | Inherent to unvalidatable resources (single-stream has same exposure); window is wider with parallelism (M1-c2 R2 P2-5) |
| B27 | Route 1 stickiness: worker-fatal `resource total changed mid-download` and 416-on-shrunk-resource could also downgrade (`SingleStreamRequired`) letting single-stream re-settle the total from response headers | M2 | Edge-of-edge: needs a size-changing server test rig to land honestly (M1-c2 R2 P2-1 tail) |
| B28 | `CURSOR_PERSIST_BYTES = 1 MiB` is hardcoded: mid-flight cursor persistence is untestable on small files (cancel tests see done==0) and coarse for small real downloads — make the threshold configurable (per-task or cfg) | M2 (scheduler) | M2-b R2 P2-7: `cancel_mid_swarm` test actually verifies row-kept + clean-resume, not mid-flight cursor retention |
| B29 | Cancel-vs-completion-proof race: biased select discards the last ready worker result on cancel, leaving a cursor-complete row in the store — next resume self-heals via completion proof, but deserves a pinning test | M2 | Verified correct by inspection (M2-b R2 P2-7); test-only gap |
| B30 | `tokio::select!` with a moved JoinHandle detaches the worker when the other branch wins — audit remaining select sites for the `&mut h` pattern when more joiners appear | M2+ | Root cause of M2-b R2 P0-1 (ghost worker leak); pattern is now documented at segment.rs join loop |
| B31 | ✅ RESOLVED (M2-d R2 fix round): `DownloadPort::purge(url, sink)` added; `Scheduler::remove` drops engine-side task+segment rows after the task row delete (test `remove_purges_engine_rows_and_is_idempotent`). Residual: `resume_job` still sends `validator: None` (no If-Range/ETag revalidation on resume) — open as B36 | M2+ | Re-add of the same target now starts fresh; B36 covers etag drift |
| B32 | `Store` has no `busy_timeout`; two `Store::open` on the same file → SQLITE_BUSY storms. Assembly rule: the daemon must CLONE one shared `Store` instance into `TaskManager` + `HttpAutoPort`, never open twice | M2-d (server wiring) | Latent wiring footgun; document + assert in wiring tests (M2-c R2) |
| B33 | `tm.complete/fail` transient `Storage` error leaves the row `Running` until next boot's crash recovery re-queues it (worker logs, no in-process retry). PANIC side is resolved (1524655: JoinHandle supervisor logs + best-effort `fail()` write) | M2+ | Boot recovery is the designed net for storage errors; add bounded retry when a real incident pattern appears |
| B34 | 200-replay double-count: server ignores Range → engine truncates + rewrites full body (`bytes_written` = full size) → `resume_start + bytes_written` overcounts; mitigated by clamping the final reading to `total` when known. Full fix = engine reports `replayed_from_zero` flag | v2.x | Self-corrects on next resume (disk truth); clamp covers the common case (M2-c R2 P2) |
| B35 | Server crate has NO Scheduler/HttpAutoPort wiring yet — scheduler is milestone code with scripted-port tests only; first real HTTP end-to-end lands in M2-d | M2-d | Deliberate milestone split (M2-c R2) |
| B36 | ✅ RESOLVED (M5-c single-stream validators + 轮E key unification): `resume_job` threads stored validators; `upsert_validator_only` (total=NULL row, etag-only refresh); downgrade restart keys the validator row on the ORIGINAL URL (B36-R2 P2-3 drift fixed, test `downgrade_validator_row_keys_original_url`); `If-Range` Last-Modified now requires a syntactically valid RFC 7231 IMF-fixdate (`valid_http_date`) in both `from_wire` and `from_probe` — garbage dates degrade to validator-less resume instead of an always-missing header. P2-4 (mirror etag churn, e.g. FileETag INode) is WONTFIX: the row's etag refreshes after every session, so the cost is one extra safe full replay per mirror rotation — inherent to rotating originals, any key choice pays it | M2+ (next engine pass) | Paused-resume against a CHANGED remote is rare for mirrors, real for small hosts |
| B37 | WS `/events` has no hello/snapshot frame: a GUI joining after `TaskAdded` sees nothing until the next event. M3 must bootstrap REST-first then subscribe (documented), or a `hello` frame lands then | M3 | Contract note, not a bug |
| B38 | `axum::serve` error path in main.rs exits before `sched.shutdown()` + socket cleanup (stale socket self-heals next boot via probe; workers un-drained). Also `shutdown_signal` consumes only the FIRST signal — a second Ctrl-C hits the default handler. Both worth fixing when the shutdown surface gets its own pass | M2+ | Filed from M2-d R2 (P2-8) |
| B39 | UI 操作失败（add/pause/limit/remove）静默：catch 后无 toast/banner，用户不知道失败原因 | M3-c3+/M4 前的 UI polish 轮 | M3-c1 R2 P1-3 驳回时遗留 |
| B40 | 桌面壳固定端口 8420：被占时 daemon 立死，GUI 只显示连接断开。改进：spawn 前 probe 端口，冲突时换端口号或弹通知 | M3-c3+ | M3-c2 R2 P2-5（单用户本地应用，接受现状） |
| B41 | sidecar stdout/stderr 在发布 GUI 二进制里进 void（无控制台）。发布前转 log 文件（app_data_dir/peregrined.log） | M6 发布轮 | M3-c2 R2 P2-7 |
| B42 | CI desktop workflow 产物未验证 deb 依赖完整性：补 `dpkg-deb -I bundle/**.deb` 打印 Depends 确认 webkit 运行时依赖在列 | CI 首跑后 | M3-c2 R2 P1-3 残余 |

## M4-a 遗留（R2/R3 处置）

- B45: engine-hls connect 级超时（hyper legacy client 无 per-request 机制；stall
  已覆盖 body 阶段，connect 阶段靠 OS TCP 超时兜底）
- B46: AttrIter 反斜杠转义剥离（真实世界 HLS 属性值几乎无转义引号；检测已有，值未剥）
- B47: HlsEngine::probe 生产接线（daemon 不 probe；M5 MCP probe 工具 / CLI 用）
- B48: hls_port.rs port 级测试（cancel→Cancelled 映射、purge parts 目录）
- B43/B44 （既有）: HLS 段级遥测与 per-task throttle

## M4-b1.1 遗留（R2' 完整报告处置）

- B49: download_merge 初始 resolve 与 EXT-X-KEY 循环未 select cancel——半开 origin
  上取消延迟最长 60-120s（有界非挂死；follow_live 内已修，join 阶段未对齐）
- B50: fetch_part_retry 对永久 4xx（404/410）同样 3 次退避（~450ms 浪费，有界）；
  重试时已计费 bytes 二次计入 budget；EXT-X-MAP init 段无重试（与 segment 不对称）
- B51: poll_cadence_override 生效期间 td 变化不刷新 stall_after（want 恒等短路）——
  仅测试路径暴露，生产无 override

## B52-B58 — M4-c（FTP）R2 未采纳项（0eafc1d）

- **B52** FTP resume 目标缺失的报错文案对齐 engine-http B19（当前裸
  `entity not found`；scheduler 仅在 sink 存在时构造 resume，纯竞态边界）。
- **B53** SIZE 550 语义区分：文件缺失 vs 不支持 SIZE（可补 MDTM 双 550
  判定缺失，让 probe 像 HTTP 404 一样尽早报错，而不是推迟到 RETR）。
- **B54** percent_decode 的 from_utf8_lossy 静默替换非 UTF-8 序列
  （凭据/路径损坏无报错）→ 需可配置策略或显式错误。
- **B55** 畸形 URL / login 530 的错误分类细化（Network → 更准的
  UnsupportedUrl/专用文案；与 engine-http 既有口径统一处理）。
- **B56** "short body" 措辞：远端在 SIZE 与 RETR 间增长时 X>Y 的文案。
- **B57** FTPS（AUTH TLS）支持（B46 关联，suppaftp async-secure feature）。
- **B58** FTP 分段并行（多控制连接 + REST 分片；服务器兼容性差，v2 研究）。
- B59 (M4-b2 R2' P2): 跨重启的非 magnet BT 行 purge 删不掉数据——registry 是内存态，重启即空；`.torrent` 行的 hash 无法从 url 推导，只能 remove_sink 兜底。修法：任务完成时把 infohash/torrent 名落任务行，purge 据此定位数据目录。
- B60 (M4-b2 R2' P2): BT per-task 限速未接（librqbit SessionOptions.ratelimits 支持全局不对称限速，per-torrent 无一等 API）；daemon 侧已 warn 拒绝，接入待上游或轮询节流。
- B62 (轮C R2 P2): fetch_part_retry 未收敛到 retry_blips（BACKOFF 常量与 arm 顺序双份维护，差异仅在成功路径读 part 长度）。
- B63 (轮C R2 P2): live 边缘——encoder 先写 playlist 后写 media 的短暂 404 现立即终止录制（旧 3 次重试后同样终止，时间差 ~450ms，非回归）。
- B64 (轮D R2 P2): sidecar log 跨会话无界增长（append 无轮转；阈值轮转方案）。
- B65 (轮D R2 P2): banner 状态机抽 lib/banner.svelte.ts + fake-timer 单测；open_sidecar_log 抽纯 FS 函数 tempdir 单测。
- B66 (轮D R2 P3): desktop.yml 无 pipefail，dpkg-deb -f 自身失败时报误导文案（step 仍红，可容忍）。
- B61 (M6-a): cargo-dist / systemd unit / 签名（deb 已有；CI 发布链路 M6-b 落）。
- B67 (Dependabot/RUSTSEC-2024-0429): glib 0.18.5 被 tauri 2.x Linux 链（gtk 0.18/webkit2gtk 2.0/tray-icon 0.24）锁死，GHSA-wrw7-89jp-8q8g 修复版 0.20.0 不可达——已在 .github/dependabot.yml ignore（范围 `>= 0.15.0, < 0.20.0` 自动到期）。暴露面≈0：仅 `Variant::str_iter()` 触发 UB，代码树无调用点。**移除触发器**：Tauri 3（或任意 tauri 版本切到 gtk-rs 0.20 链）后删除该 ignore 块，随 tauri 升级一并带 glib ≥0.20。tauri 上游 issue #15035/#12048。
