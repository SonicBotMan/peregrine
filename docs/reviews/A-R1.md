# 轮A R1 自查（daemon 健壮性：B38 + B13 + B24 + B34）

对象：未提交 diff（17 文件，+338/−12）。逐项自查后交 R2 独立审查。

## B34 — 200-replay 双计数

- **变更**：`DownloadOutcome` 新增 `replayed_from_zero: bool`（api/download.rs）；engine-http 主构造点按 `resume 带正 offset && WriteMode::Truncate` 置位；416-heal 递归返回点**无条件**覆写 `true`（caller 视角 offset 被丢弃）；segment/hls/ftp/bt 四个构造点 `false`。scheduler `finish` 调用点按 flag 重基 `resume_base = 0`。
- **自查**：
  - 递归内层 resume=None 永远 false → 外层覆写不会被内层意外清除 ✓；内层 416-settled 分支要求 resume 存在，healed 内层不可达 ✓。
  - `(Some(ctx), 206)` 校验 start 匹配后 Append → false ✓；`(None, 200)` fresh 下载 → resume.is_some()=false → false ✓（定义如此）。
  - finish 原有 `min(total)` clamp 保留，双保险 ✓。
  - HLS/FTP/BT/segmented 引擎语义上不存在 caller-offset 丢弃场景，`false` 是正确常量 ✓。
  - 测试：validator_resume 两条既有用例加断言（206 → false；changed-remote 200 → true + bytes_written=全量）；scheduler 新增 `replayed_from_zero_session_does_not_double_count_resume_offset`（1000 partial + 5000 replay → received=5000 而非 6000）✓。
- **存疑点（交 R2）**：auto.rs Route 2 → run_download_impl 透传 outcome 未显式检查（auto.rs 只透传，无构造点）；`resume_start` 在 scheduler 里取自 job.resume（resume_job 用 sink 文件长度），engine 内部 Truncate 分支与 resume_start 来源一致性。

## B38 — shutdown 双信号

- **变更**：`shutdown_signal` 第一信号后不返回，park 在第二个 select 上（SIGINT/SIGTERM 各自 handler），第二信号 → warn + `exit(130)`。
- **自查**：exit 跳过 socket 清理是有意的（uds::bind stale 自愈）；doc 注明；`#[cfg(not(unix))]` 分支 pending 对齐原函数签名 ✓。无测试（信号语义集成级，e2e 覆盖）。

## B13 — socket hardening

- **变更**：
  1. `uds::bind` umask 0o077 包住 `UnixListener::bind`（bind→chmod 窗口关闭），bind 后恢复原 umask。
  2. `xdg_runtime_dir_ok`：XDG 目录须存在、是目录、uid==本 uid、无 group/other 写位，否则 fallback /tmp。
  3. `tmp_fallback_dir`：symlink_metadata 检测 fallback 路径为 symlink → PermissionDenied 报错（名字内嵌本 uid，合法场景不可能是 symlink）。
- **自查**：
  - umask 是进程级，async 上下文改它理论影响并发创建文件的其他线程——启动早期 bind 路径，窗口 µs 级且方向是收紧（错误方向只会让别的文件更严），注释已声明；R2 请重点评估。
  - `metadata()`（跟随 symlink）验证 XDG 最终目录 vs `symlink_metadata`——有意：XDG 本身常是 symlink（systemd 场景），验证的是落点目录属性。
  - xdg 不合格静默 fallback（不 warn）——**存疑**：用户显式设了 XDG 却被忽略，应 warn？交 R2。
  - 测试：xdg 单测覆盖 0700/0770/0702/不存在/非目录 5 分支 ✓；umask 无单测（进程级副作用，集成验证）。
- **未做**（维持 B13 记录）：uds.rs stat→unlink TOCTOU——remove_socket_file 已有 liveness+identity 双检，残余风险已文档化。

## B24 — 孤儿行 GC

- **变更**：storage 新增 `purge_missing_sinks()`（startup 用）：扫 tasks 全表，sink 文件确认 NotFound 或非普通文件 → 删行（FK 级联 segments）；其他 io 错误（如权限）→ 保留该行。daemon.start() 在 boot() requeue **之前**调用，best-effort（失败 warn 继续）。
- **自查**：
  - 只在 startup 跑 → 无活动下载，不存在「行刚写、文件将建」的竞态窗口（该窗口由下一次启动的 GC 正确清理）✓。
  - validator-only 行 + sink 存在 → 保留（B36 跨重启语义保住）✓；sink 没了 → 删除（无从续传，validator 无意义）✓。
  - metadata 返回目录（用户把 sink 路径建成目录）→ `!m.is_file()` → 删——合理？该行永远无法 resume。**存疑**：是否过激，交 R2。
  - blocked-pool 单连接 Mutex 内做 fs stat——每行一次 syscall，行数量级小（百级），µs 级；不构成启动瓶颈 ✓。
  - 测试：三行场景（segmented 文件删 / validator-only 文件在 / 文件从未存在）→ 恰删 2 ✓。
- **未做**（B24 原文里的 redirect-变体孤儿）：sink 还在但 redirect target 变了的行无法识别为孤儿——本轮 sink-missing GC 覆盖主要膨胀源，TTL/GC-by-redirect 留 backlog。

## 全局

- workspace 43 套全绿（一次 HLS `cancel_midway` 偶发 flake 复跑 3 连绿，非本轮触碰面）；clippy 0；fmt 过。
- Cargo.toml 变更：server 加 libc、api 加 dev tempfile——均 workspace 依赖，无新版本引入。

R1 结论：可交 R2。
