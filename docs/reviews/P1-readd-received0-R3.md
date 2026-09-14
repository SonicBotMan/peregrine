# P1 修复轮 R3 — 复盘反思（received=0 / seed-lift）

日期：2026-09-15 · 前置：[R1](P1-readd-received0-R1.md)（方案）· R2 delegate 报告（del_mu1jnf88_lpgl，工作副本于提交消息引用）

## 三轮结论

- **R2 结论：无 P0，同意提交**；P1×2 + P2×3，全部采纳并落地（见下）。
- revert-red 实证：stash 修复后 H1 落 received=0、H2 落 `40000 != 100000`（bug 症状原样复现）→ 测试判别力成立；恢复后全绿。

## R2 发现与处置

| 级别 | 发现 | 处置 |
| --- | --- | --- |
| P1-1 | 「speed stays real」是无守护过声称：60k 磁盘真实现在**首个** publish（非中段），速度语义靠「首帧基线重置」而非「无跳变」 | ✅ H2 测试补事件形态断言（首 publish ∈ [60k,70k]；后续增量 ≤ 2 帧）+ R1 文档口径修正 |
| P1-2 | `on_session_base` 二次调用且 base₂ 更小时 clobber base → rebase 通胀可超 total（`SingleStreamRequired` 降级是真实路径） | ✅ base 写回改单调 `old.max(base)` |
| P2-1 | lift 的 clamp 复合在顺序路径退化为 `st.0 = base`，注释与行为矛盾 | ✅ 注释重写（`t.max(base)` 语义：磁盘真相压过陈旧 total；乱序路径才起钳制作用） |
| P2-2 | seed 的 Relaxed 依赖「全在 state 锁内」不变量，无注释固化 | ✅ 字段注释补锁序说明 |
| P2-3 | base > 陈旧 total 时 GUI 可短暂显示 >100%（触发苛刻、纯展示） | 📌 backlog（GUI 侧可按 total 钳显示值） |

最终：**265/265 绿**（263 原有 + 2 新增；速度断言并入 H2 不另计）· clippy -D warnings 0 · fmt 净。

## 反思（为什么 R1 会过声称）

1. **「修复三个症状」的表述惯性**：坐标系论证对 received 语义严谨，但把「幻影速度」也归入战果时没追问「速度在哪里计算」——速度在 GUI 消费侧从相邻事件差分，sink 只保证 received 正确。**教训：跨层修复声称必须落到计算发生的层去验证**。R2 用「sink state 里没有速度字段」一句话戳破——自审时应先问「我修的层拥有这个语义吗」。
2. **契约级地雷靠 R2 抓住**：`on_session_base` 是公开 trait 方法，单次调用是 4 个引擎的**现状**而非**契约**；降级路径（同 sink 二次调用）就在本仓库 `run_with_downgrade` 里。R1 把「多次调用」标记为「契约外乱序」弱化处理，R2 实证它是可达路径。**教训：trait 契约按「允许的事」设计，不按「今天没人这么用」设计**。
3. **时序假设未经实验就写断言**：速度形态断言第一版假设帧会分离 publish，实跑只有 1 帧终值（drainer 250ms 合并了瞬时脚本）。加 `frame_pause(300ms)` 后才成立。**教训：断言涉及时间粒度时，先跑再信**。
4. 方案收敛（三层→单点）与弃 A/B/C 的论证经受住了 R2 推敲——**先找坐标系错位、再动架构**的顺序是对的；A（purge 重下 36MB）这种「用带宽换省事」的方案在审查清单里就该被否。

## Backlog（本轮不动）

- P2-3：GUI 对 >100% 短暂显示的钳制（若真实出现）。
- dependabot moderate（glib 0.18.5，桌面壳传递依赖）：被 tauri/gtk 钉死 0.x，待上游适配 glib 0.20 后升级；主交付链（daemon/CLI/MCP）不受影响。
