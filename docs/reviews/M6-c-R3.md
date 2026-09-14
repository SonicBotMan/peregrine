# M6-c R3 终审反思（第 3 轮）

前置：R1（docs/reviews/M6-c-R1.md，2 P1 + 2 P2 已处置）、R2 独立审查（del_mu1es3kl_hcv3，报告与处置见 M6-c-R2-disposition.md）。

## 交付矩阵

| 交付物 | 验证 |
| --- | --- |
| `pg completions bash/zsh/fish` | clap_complete，shell 枚举全量 |
| `pg gen-man`（隐藏） | man -l 渲染验证 |
| TCP 客户端 | `Endpoint::parse_checked`（tcp:8800/tcp:host:port/拒绝畸形）+ `BoxedStream`（hyper::rt 委托 TokioIo，绕孤儿规则）+ authority 同源（TCP 用真实 host 过 daemon 的 DNS rebinding guard） |
| systemd 单元 ×2 | 用户级 UDS（真机 minimal active + ping ok）/ 系统级 TCP 模板（@端口实例） |
| README 安装节 | 与全部命令实测一致 |

## SIGTERM 时序验证（本轮预研）

`SIGTERM → axum graceful drain（in-flight HTTP）→ sched.shutdown()（cancel → drain workers）→ uds unlink`。
`drain()` 无内部超时；兜底是 systemd `TimeoutStopSec`（默认 90s）+ SIGKILL——安全，因为 resume 状态=durable queue，被 SIGKILL 的 `Running` 行由下次 boot 的 crash recovery re-queue（scheduler/src/lib.rs:470 注释即此契约）。`RestartSec=3` 与之兼容。

## 三轮复盘

1. **R1 抓质量、R2 抓契约**：R1 的两 P1（parse 零覆盖、ProtectHome 矛盾）是单文件质量问题；R2 的发现集中在跨组件契约（见 disposition）。三轮各有盲区，缺一不可。
2. **write 静默回滚已第 4 次发生**（本轮 assets/ 单元文件）：报成功实未落盘。操作纪律已固化：落盘后必 `wc -c`/md5 验证。
3. **容器 vs 真机 systemd 语义差**：沙箱指令组在容器 user manager 下被 seccomp 拒绝（status=218），真机正常——部署文档必须写「最小单元真机验证过」，避免容器用户误判单元损坏。

## 结论

三轮完成：R1（2 P1 + 2 P2 已处置）、R2（2 P1 + 7 P2 全部采纳修复，见 M6-c-R2-disposition.md）、R3（本文档）。

- 263/263 绿、clippy 0、fmt 干净
- 端到端实测：TCP 双形态 ping ✓ / 常驻 WS + SIGTERM 102ms 退出 ✓ / MCP-over-TCP 全链路 ✓
- 两个 P1 均在 systemd 交付物侧，代码侧核心（BoxedStream/Endpoint/guard）经 R2 独立验证无误

**M6-c 通过终审，可提交。**
