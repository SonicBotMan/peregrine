# QA-E2E R2 复审处置（del_mu2gwmk0_a221）

R2 结论：三个 bug 定性全部成立（P0/P1/P1），但 R1 两处根因归因错误，并有 4 项衍生遗漏。
**R1 勘误与处置全部采纳**。修复优先序（采纳 R2 建议）：

| 序 | 项 | 方案 | 修复落点 |
| --- | --- | --- | --- |
| 1 | Bug 3 + 416-silent-corrupt 变体 | 416 自愈：非"已完整"判定的 416（start_offset>0）→ 截断 sink、Replace 从 0 重下（有界一次） | engine-http/src/download.rs:236-264 分支扩展 + 单测 |
| 2 | Bug 1（P0） | **方案 B**：DownloadPort 加 `finalize(url,sink)` 默认 no-op；worker Cancelled 分支（scheduler/lib.rs:753-760）调用；hls_port 实现为 **gap-tolerant** merge（seq 升序拼接、容忍空洞、成功删 .parts、失败保留+log）；ENDLIST 干净路径仍走严格 merge_parts；CLI 文案更新 | scheduler/{lib,hls_port,ftp_port,http port}.rs + cli |
| 3 | Bug 2 | **方向 a**：hls_port/ftp_port 照抄 HttpAutoPort locals 模式（row seed + live poke，~40 行/port；引擎已 honor budget 零改动）；`pg limit` 对 BT 任务显式报错；吞吐收敛集成测试 | scheduler/{hls_port,ftp_port}.rs + server/cli + 测试 |

## R1 勘误（采纳）

- **Bug 2 根因更正**：engine-ftp（lib.rs:371/389 `budget.slice_hint/acquire`）与 engine-hls（fetch.rs:153-154）**均 honor budget**——R1"引擎无限速逻辑"为 grep 关键词误判（限速原语叫 budget 不叫 rate/bps）。真根因：per-task local bucket 只在 HttpAutoPort（scheduler/lib.rs:200-272 budget_for+locals+set_task_limit 活 poke）；hls_port.rs:40-44 / ftp_port.rs:38-46 local 恒 unlimited，set_task_limit no-op（hls:103-105/ftp:72-74），daemon RoutingPort 扇出后被静默吞掉。row 持久化链路通（scheduler/lib.rs:415-421→tm.set_limit）→"显示有限速实际无效"。
- **Bug 3 归因更正**：sparse 预分配 `.set_len(total)` 在 engine-http/src/segment.rs:348，由 **PR #107** 引入；#146 未触碰 segment.rs。R1 记 #146 有误。
- **Bug 3 新变体（P0 级）**：若镜像 416 带 `Content-Range: bytes */T` 且 T==start_offset，download.rs:243-247 判"已完整"→ sparse 坏文件静默标 Completed（数据损坏）。QA 的 Yandex 镜像未发该头才表现为 failed。修复须同时处理两分支。
- **Bug 1 衍生**：Cancelled 分支跳过所有终态写入；严格 merge_parts 遇段号空洞报 SegmentGap，live salvage 必须 gap-tolerant；download_merge merge 前 drop progress sink（engine-hls lib.rs:269-272），finalize 期间进度静默可接受（记 log）。
- **Bug 2 补漏**：engine-bt 连 global budget 都未接（src/ 零 budget 引用）——本轮按 c 方向显式报错，接线留 backlog。
- **CLI 文案**（cli/src/main.rs:55）"partial files are kept"只对一半：HTTP 任务 segment rows 被 B31 无条件清除（scheduler/lib.rs:236-260 注释）——正是 Bug 3 触发器，文案需改。

## 机制锚（修复时直接引用）

- resume_job len() 污染源：scheduler/src/lib.rs:781-793
- Range 发出点：engine-http/src/download.rs:222-224；on-disk 校验 :352-367（len==start_offset 形同虚设）；假进度 seed :216 on_session_base
- B31 触发链：pg remove(purge=false) 保 sparse 主文件删 segment rows → 重加同 URL/sink（tm lib.rs:43 只拦 active）→ auto Route 2 单流 → len()=792,723,456 → 416
- Bug 1 cancel 出口全景：engine-hls lib.rs :133/:262/:356/:419/:437/:445/:453/:475/:522/:545；merge 唯一调用点 :273；merge_parts :591（健全：tmp+rename、.hls-merging 原子）
- 方案 B 风险三条：shutdown 30s drain 可能掐长 merge（安全：.parts 保留）、remove 返回时产物未就绪（CLI 提示 finalizing）、finalize 进度静默（接受）
- Bug 2 实现风险：locals 必须存共享句柄（Arc/内部可变）而非 clone，否则 set_bps 不传播
