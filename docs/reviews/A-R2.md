现在我来查看一下剩余的差异和 `auto.rs` 的透传：现在让我检查一下这些修改周边的完整上下文：main.rs 关闭流程、daemon 启动顺序、uds.rs、存储内部结构以及 schema：main.rs 中 B38 的交互至关重要——我需要检查在 `serve` 返回之前，究竟是誰在观察 `daemon.cancel`，以及调度器是如何关闭的：`daemon.cancel` 仅驱动 WS 流；`sched.shutdown()` 在第 4 步运行 —— 在 `serve` 返回**之后**。在 B38 的情况下，`shutdown_signal`（即 axum 的优雅关闭触发器）在第一个信号上不再解析。让我通过实验验证这一关键交互：Daemon 未启动（退出码 2 —— 可能是 CLI 参数错误）。我来检查一下实际的 CLI 接口：**经验证确认 P0 问题**：单次 SIGINT 信号使 daemon 处于存活状态（僵尸状态）；只有第二次信号才会触发 `exit(130)` 退出。我来验证一下变更前HEAD的行为，并检查 e2e 是如何停止 daemon 的：e2e 测试框架只执行了 `kill -9` / 即发即忘（fire-and-forget）的 `kill` —— 没有测试优雅退出（graceful exit），这解释了为什么 43 个测试全部通过（绿色）。现在针对 B24 的关键问题是：这个存储 `tasks` 表中到底包含什么 —— 仅仅是引擎状态，还是也包含了面向用户的任务列表？`tasks` 表仅用于引擎簿记（段/验证器）；用户任务列表存在于独立的 `downloads` 表中。现在让我抽查一下 FTP 引擎（它具有类似 REST 的恢复语义 —— `false` 在那里是正确的常量吗？）以及其他存储方法是如何锁定以进行比较的：FTP 有一个真实的磁盘重写路径 (`(Some(_), _) => (0, true)` — “HTTP 200-replay semantics”) 但报告了 `false`。让我检查一下它是如何报告总数据的，并检查 HLS 对 `job.resume` 的处理：证据已收集完毕。正在撰写 R2 审查报告。

---

# R2 独立审查报告 — daemon 健壮性轮（B34 / B38 / B13 / B24）

对象：未提交 diff（17 文件，+338/−12），对照 docs/reviews/A-R1.md。只读审查，未改任何文件。

## P0 — 阻断级

### P0-1 B38：`shutdown_signal` 停靠（park）后阻塞了 axum 的优雅退出触发器 — 单信号停机功能已损坏
`crates/server/src/main.rs:223-242`（配合 `main.rs:130`、`main.rs:145`）

- axum 0.8 的 `with_graceful_shutdown(fut)` 语义是：**fut 完成时**服务器才停止接受新连接并进入 drain，serve future 才 resolve。原实现首信号 → `cancel.cancel()` → 函数返回 → drain 开始。新实现在 `cancel.cancel()` 后停靠在第二个 select 上等待第二信号，**future 永不返回**（除非再来一信号）。
- 后果链：
  1. 首信号后 axum serve loops **继续接受新连接**（drain 根本没开始），`main` 卡在第 3 步；
  2. 第 4 步的 `daemon.sched.shutdown()`（引擎 drain）和 `remove_socket_file` **永远不可达**——它们只在 serve future 完成后运行，而唯一能让它完成的方式是第二信号触发 `exit(130)`，而 `exit(130)` 恰恰跳过这些清理；
  3. 调度器也未停：首信号只杀了 WS 事件流，**下载继续跑、还能接受新任务**——daemon 对停机请求实际"不理会"。
- 交互场景：`systemctl stop` 只发一次 SIGTERM → token 取消、WS 断开，之后 daemon 挂着等第二信号直到 `TimeoutStopSec` → **SIGKILL**（无任何清理、退出码 143/SIGKILL）。交互场景用户必须 ^C 两次才能退，第一次表现为"没退"。这比改动前（单信号 → 全链路优雅清理）是全面回归。
- `#[cfg(not(unix))]` 分支同样从"返回"改成 `std::future::pending::<()>().await`（main.rs:241-242）——非 unix 平台同样永久停靠。
- 注释自称"FIRST signal … starting the graceful drain"（main.rs:228-229）——对 axum 语义的理解是错的，drain 由 future 完成触发，不是由 cancel token 触发。
- **修法建议**：保持 `shutdown_signal` 在首信号后照常返回（恢复 drain 触发），把"第二信号强制退出"改为独立 `tokio::spawn` 的看门狗任务（先 `cancel.cancel()`，再等第二信号，到点 `exit(130)`）。这样单信号语义恢复、双信号强制退出保留。
- 另注：`ctrl_c` future 已完成到新 `signal()` 流注册之间有 µs 级窗口，期间到达的第二信号会被 tokio 全局 handler 静默吞掉（不落盘、不退出、也无日志）——窗口极小，随 P0 修复一并消失（spawn 后先注册再 cancel）。

## P1 — 应修

### P1-1 B13：XDG_RUNTIME_DIR 显式设置但校验失败时静默 fallback /tmp，无任何日志
`crates/api/src/transport.rs:74-81`

- 用户/系统管理员显式设置了 XDG_RUNTIME_DIR，却因 group-writable、属主不符等原因被拒——运行时目录被静默改到 `/tmp/peregrine-<uid>/`。客户端与服务端走同一 `default_socket_path()` 所以功能上自洽，但这是**安全相关的拒绝降级**，运维完全不可见：XDG 配置错了没人知道，socket 落在意外位置也没人知道。
- 建议：校验失败时 `tracing::warn!`（说明拒绝原因和 fallback 路径；tracing 未初始化时是 no-op，客户端侧安全）。R1 存疑点 1 的答案是：**应该警告（warn）**。

## P2 — 建议

### P2-1 B38：`exit(130)` 对 SIGTERM 路径语义不准
`crates/server/src/main.rs:239` —— 128+SIGINT=130，但第二信号若是 SIGTERM，惯例是 128+15=143。systemd 会把非零退出当失败记录。区分两分支各用各自码，成本一行。

### P2-2 B38：`signal()` 注册失败在停机路径上 panic
`main.rs:230-234` 两个 `.expect(...)` —— fd 耗尽等极端场景下停机路径 panic。随 P0 重构（spawn 看门狗）改为 warn + 放弃强制退出即可。

### P2-3 B24：`purge_missing_sinks` 逐行 DELETE 无事务包裹
`crates/storage/src/lib.rs:343-346` —— 每行一条自动提交（autocommit）commit（各自 fsync）；中途出错则半删（可接受：幂等、下次启动重跑）。建议包一层事务：原子、且 N 行只一次 fsync。行数百级时是优化项非正确性项。

### P2-4 B34：缺一条 `on_session_base` + 200-replay 组合的回归测试
`crates/scheduler/tests/scheduler.rs:637-686` —— `OkReplayed` 脚本**不声明 session base**，而真实引擎在 session 入口就调 `on_session_base(start_offset)`（engine-http/src/download.rs 顶部）。我推演过该组合是安全的（`finish(0, bytes_written)` 经 `rebase` 后落在正确的绝对值；shrink 场景由 `st.0.min(total)` 钳住，lib.rs:1111-1113），但这条不变量目前只靠推理守护，建议给脚本加一个"先声明 base=1000 再 OkReplayed"的用例钉死。

### P2-5 B13：umask 修改的 panic 窗口（极低风险，记录即可）
`crates/server/src/uds.rs:66-70` —— `umask(0o077)` 与恢复之间是同步序列、无 await，bind 返回 Result 不 panic，实际窗口仅剩"bind 内部意外 panic"会让 0o077 泄漏为进程级——可忽略。R1 的风险评估成立，结论：无问题。

## R1 四个存疑点逐条裁定

| 存疑点 | 裁定 |
|---|---|
| ① XDG 不合格静默 fallback 是否该警告 (warn) | **该警告 (warn)**，升为 P1-1 |
| ② sink 是目录时删行是否过激 | **不过激**。目录在 sink 路径上意味着 `File::create` 永远 EISDIR，行无任何续传价值；且删除的只是 engine resume 行（键为 url+sink），不影响用户任务行，re-add 走全新路径。`!m.is_file()` 同时正确覆盖 FIFO/device/悬空 symlink（metadata 跟随 symlink 与引擎 `File::create` 行为一致） |
| ③ umask 进程级修改在 async 上下文的风险 | **评估成立**。umask→bind→恢复之间无 await（uds.rs:66-70 同步序列）；时序上 main 先 bind 监听（main.rs:62-90）再 `Daemon::build` 建 DB，窗口内无其他文件创建者；即便有，方向是"多收紧"。错误路径也已把 `?` 移到 umask 恢复之后，bind 失败也会恢复。无问题 |
| ④ scheduler `resume_start` 与 engine `replayed_from_zero` 判定一致性 | **一致**。lib.rs:836-837 `resume_job` 构建 job、同一条 `job.resume` 既喂 engine（lib.rs:849-852）又提取 `resume_start`，单一数据源。engine 侧 `resume.is_some() && start_offset>0 && Truncate` 与 scheduler 侧 flag 消费完全对齐；`start_offset==0` 时两边都退化为同一结果。auto.rs 无构造点、纯透传（grep 确认全部 6 个构造点都改了；validator_resume.rs 用 `download_auto` 断言 replayed=true 直接覆盖了 Route 2 透传） |

## 重点问题 b/c/d/e 裁定

- **b) 416-settled 分支 `replayed_from_zero: false` 正确**（download.rs:281）。该分支 offset==total、无字节重写，offset 依然有效；finish 走 `rebase(resume_start + 0)` 落在 total ✓。若置 true 反而依赖 `max(st.0,…)` 碰巧兜底，语义错误且脆弱。当前取值正确。
- **c) purge SQL/错误处理**：`spawn_blocking` 内 `Mutex` 持锁做 `fs::metadata` —— 启动期、serve 之前（main.rs 时序 bind→start→serve）、单调用方，无竞争；syscall 百级 ×µs 不构成瓶颈，**合理**。`NotFound → 删`、其他 io 错误 → 保留的分流正确（权限错误不误删）。「非普通文件」判定见上表②。唯一改进是 P2-3 的事务包裹。daemon.rs:294-309 的调用点（boot 前、尽力而为 (best-effort)）时序正确——purge 先于 boot() requeue，requeued 任务不会粘上已删行。
- **d) 第二信号路径**：`exit(130)` 跳过 `sched.shutdown()` 和 socket 清理——作为"用户失去耐心"的强制出口是可接受的（uds stale 自愈已文档化，sqlite WAL 崩溃恢复安全）；**真正的问题不在 exit 本身而在 P0-1：它成了唯一出口**。`#[cfg]` 分支编译正确但非 unix 分支的 `pending` 同样引入 P0 停靠。
- **e) lock/await、错误吞掉、语义回归扫描**：
  - 持锁跨 await：未发现新实例（sink 的锁均已短临界区；purge 的锁在 blocking 线程内无 await）。
  - 错误吞掉：purge 对 transient stat 错误"保留不删"是有意设计且正确；其余错误路径均有日志。
  - 语义回归：除 P0-1（B38）外未发现。B34 的 finish 重基在 206-append / 200-replay / 416-heal / 416-settled 四条路径推演全部落点正确，shrink 场景由既有 `min(total)` 钳位兜住（lib.rs:1111-1113）；replay 中途行读数冻结在旧值属单调 max 既有语义，非回归。
  - 测试与构建声明与 diff 一致（validator_resume 两断言、scheduler 新用例、storage 三行 GC 用例均覆盖核心行为；Cargo.lock 仅 tempfile(dev)/libc 两处，均 workspace 已有，无新版本引入）。

## 结论

**需修复后提交。**

- P0-1（B38 单信号优雅停机回归）必须修复——修法小（看门狗 spawn 化），但它是 systemd 停机路径的硬回归，不能带病提交。
- P1-1（XDG 拒绝静默）建议随同修复，一行 warn。
- P2 各项不阻断，可随本轮或 backlog 处理。

B34、B13、B24 三个 backlog 项的实现质量本身是过关的：判定逻辑一致、错误分流正确、测试钉住了关键不变量；B13 的 umask 与 symlink 防护、B24 的 GC 时序与保守错误处理均无发现问题。