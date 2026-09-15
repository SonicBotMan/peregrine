# 契约修复轮 R1 — backlog 两项（完成态 segments 空 + pg list id 截断）

日期：2026-09-15 · 范围：`engine-http/src/segment.rs`（完成路径）、`cli/src/main.rs`（print_table）、测试四处

## 问题 1：完成态 `/tasks/{id}/segments` 返回空（GUI 验收发现）

**根因**：`run_segmented_download` 成功完成时 `store.delete_task(task_id)` 把段行删了；daemon 的 `segments_of` 是 store→wire 透传 → 完成态必空。

**修复（单点，责任链已存在）**：删掉完成路径的 `delete_task`，行保留为终值。三层语义自洽：

1. **遥测**：完成态段表 = 终值（全 done），GUI 段面板不再清空
2. **重加即秒完**：同 (url, sink) 重添加 → resume 路径发现全 done → 跳过全部 → `on_session_base(total)` seed-lift → 立即完成（与 P1 修复轮 H1 语义闭环）
3. **清理有主**：任务 remove → scheduler `purge()` 永远删行（B31+storage 级联，M5.1 已落地）

**陈旧行安全性**（不删行不等于泄漏）：文件被手动删 → resume sink-length check replan；内容变了 → etag 不匹配 replan（M1-c1 R2 P0-1/P2-2 既有防护）。DB 增长：每任务 ~9 行 SQLite，桌面量级无压力；BT/HLS 引擎不用 Store 段表，不受影响。

## 问题 2：`pg list` id 列截断

id 是 21 字符（`{16hex}-{4hex}`），旧 `take(14)` 截掉 7 字符 → 截断后的 id 无法用于 `pg get/pause/remove`。

**修复**：id 全长不截断（操作句柄不可截）；URL 列吸收宽度预算——`COLUMNS` env（TTY shell 都导出）fallback 80；`clip_url` 头部截断保尾（文件名是眼睛要的）；URL 宽度下限 8（极窄终端仍可读）。纯函数 `url_width`/`clip_url` 抽出，各带单测。

## 验证

- engine `completion_keeps_plan_rows_for_telemetry_and_readd`：完成 → 行在且全 done；**重加 → completed + `bytes_written == 0`**（零请求秒完，跑过为真）
- 旧断言反转两处（segment.rs/auto.rs 的「完成后行必删」→「行保留且 terminal」）
- CLI +2 单测（`url_width_budgets_full_id_and_floors_url`、`clip_url_keeps_tail_head_first`）
- **268/268 绿**（265+3）· clippy -D warnings 0 · fmt 净
- server 层不另加测试：rig 是 DownloadPort mock 不产生真 store 行；daemon.segments_of 透传已有专门测试（`segments_view_reflects_stored_plan_rows`），端到端由引擎级覆盖

## 已知权衡

- 完成行保留使 DB 行数随历史任务线性增长（remove 即清）。若未来要自动 GC，应在 task-manager 层加 LRU 清理，不动引擎完成路径（保持「完成态可查」契约）。
- `COLUMNS` 未导出时（纯 pipe）按 80 宽；不引 terminal_size crate（零依赖原则，hyper-util 无 TIOCGWINSZ 路径）。
- **（R2 P2-1）外部改动 sink 长度 → 重加硬错误不 replan**：旧行为（行已删）重加会全量重下；新行为下被截断/追加的文件触发 `resume mismatch` fatal（需手动删文件）。这是 M1-c1 R2 P0-1「不掩盖外部干预」设计的暴露面扩大，有意保留。文件被删（NotFound）仍 replan 自愈。
- （R2 P2-2）clip_url 按 chars 计数，CJK/emoji URL 会低估显示宽度——实践中 URL 恒 ASCII（IDN punycode/percent-encode），不引 unicode-width 依赖。
- （R2 P2-4）重加秒完的 daemon 全链路（Route 1 短路 × resume_job）无 server 级集成测试（rig 为 mock port），engine 直调已覆盖核心语义，记后续。

## R2 复审结论（del_mu2bnvx7_rmjw）

**同意提交，无 P0**。P1-1：秒完路径 `set_validator(None)` 抹空存储 etag 且与注释矛盾 → 已修（`last_etag.or(final_state.etag)`）+ 注释改为如实陈述（etag 防线在生产重加链路不可达，two-ended 206 + total guard 是主防线）。P2-3：`id_w` 改 `chars().count()`（防御未来 id 形态）。P2-1/2/4 已记权衡节。revert-red 实证成立（stash 后新测试即红）；全消费者 7 处排查自洽；崩溃恢复意外受益（完成→tm.complete 前崩溃，boot 秒完而非全量重下）。

## R2 待办

delegate 复审：行保留与 auto.rs 降级路径（`delete_task` 后 replan）的交互、重加零请求断言的时序稳健性、CLI 列宽计算的边界（unicode URL）、两处旧断言反转是否漏改其他「完成后行为空」假设。
