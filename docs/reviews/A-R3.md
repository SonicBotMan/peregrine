# 轮A R3 终审（daemon 健壮性：B34 + B38 + B13 + B24）

流程：实现 → R1（docs/reviews/A-R1.md）→ R2 独立审查（del_mu2lwjzz_1vm5，全文 docs/reviews/A-R2.md）→ 处置 → R3。
修复后验证：`cargo test --workspace` 284/0；clippy 0 warning；fmt 过。

## R2 处置表

| 发现 | 级别 | 处置 |
| --- | --- | --- |
| P0-1 B38 停靠第二信号阻塞 axum drain 触发——单信号优雅停机全面回归 | P0 | **已修**：`shutdown_signal` 恢复首信号即 cancel+返回（drain 正常触发）；双信号强制退出拆为独立 `force_exit_watchdog`，serve 前 spawn 一次（先注册信号流关闭竞态窗） |
| P1-1 XDG 显式配置被拒后静默 fallback /tmp | P1 | **已修**：`default_socket_path` 校验失败分支 `tracing::warn!`（api crate 补 tracing 依赖） |
| P2-1 exit(130) 对 SIGTERM 语义不准 | P2 | **已修**：看门狗按第二信号分支区分 130/143 |
| P2-2 signal() 注册失败在停机路径 panic | P2 | **已修**：warn + return（放弃强制退出，优雅路径不受影响） |
| P2-3 purge 逐行 DELETE 无事务 | P2 | **已修**：`transaction_with_behavior(Immediate)` 包整次 sweep，单 fsync，原子 |
| P2-4 OkReplayed 缺 session-base 声明的组合回归 | P2 | **已修**：新增 `OkReplayedAfterBase` 脚本变体 + 用例（base=1000 + replay 5000 → received=5000） |
| P2-5 umask panic 窗口（bind 内部 panic 才泄漏） | 记录 | 极低风险，不修（R2 裁定 R1 评估成立） |

R1 四存疑点裁定（R2）：① XDG 拒绝应 warn（升 P1-1 已修）；② sink 为目录删行不过激（EISDIR 永不可续传，且只影响 engine 行）；③ umask 风险评估成立；④ resume_start/replayed_from_zero 单一数据源一致，416-settled 分支 false 正确。

## 教训

1. **框架控制流语义必须验证，不能凭直觉写进注释**——B38 的 P0 源于把 axum `with_graceful_shutdown` 记成「token 取消触发 drain」，实际是「future 完成触发」；错误认知被写进注释还配了自信的理由，R1 因此自评通过。R2 靠起进程发信号实证。对「谁在等什么」类外部框架断言，review 时用可执行手段验证。
2. **R1 显式存疑机制有效**——4 个存疑点给 R2 提供了高效入口，1 个升级为 P1、3 个确认成立。存疑比全过更接近真相。
3. **脚本式测试必须对齐真实调用序列**——首版 OkReplayed 没模拟真实引擎入口必调的 `on_session_base`，不变量只靠推理守护；补组合用例钉死。
