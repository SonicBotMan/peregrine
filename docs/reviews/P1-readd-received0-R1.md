# P1 修复轮 R1 — received=0（同目标重添加）seed-lift 根治

日期：2026-09-15 · 对应发现报告：[GUI-verify-R1.md](GUI-verify-R1.md)
修复人：主 agent · R2 = delegate 独立复审（后续）· R3 = 修复后反思（后续）

## 方案收敛：三层 → 单点

设计期曾列 A（add 时 purge 全完成旧行）/ B（Route 1 过滤）/ C（完成路径无条件上报）三层。深入后收敛为**单点根因修复**：

> **sink 侧 seed-lift**：`CoalescingSink::on_session_base` 中，当 engine 声明的 base（磁盘真相：旧 plan 的 initial_done）**超过**行 seed（worker 启动时行的 received）时，把 seed 抬升到 base。

理由（弃 A/B/C 的因为）：

- 失配的本质是**一个坐标系错位**：engine 上报的是旧 plan 的绝对坐标，sink 把它 rebase（`seed + (v - base)`）到新行的 0 起点。三个症状（H1 冻 0、H2 假速度、合法 resume 首帧滞后）都是这一个公式在不同 seed/base 差值下的表现。修坐标系（seed := base 时 rebase ≡ 恒等），三个症状同时消失，且不动任何规划/路由逻辑。
- **A 的坑**：purge 段表后 fresh 路径会对已完整文件 replan 并**重下全部 36MB**（workers 不知道哪些段已在磁盘）。为省一个 rebase 公式引入带宽回归，不换。
- **B 的坑**：`auto.rs` Route 1 与 `segment.rs` run_attempt 各查一次行，双处过滤易漂移；且过滤后同样落入 fresh-replan 坑。
- **C 不充分**：bug 场景里 `initial_done > 0` 的上报**本来就发了**，是 sink rebase 把它吃掉的——上报侧没有可加的东西。

## 改动（crates/scheduler/src/lib.rs）

1. `seed: u64` → `seed: AtomicU64`（`on_session_base` 可能中途抬升；engine 报告面与 drainer 并发，Atomic 保守正确）。
2. `on_session_base`：`base > seed` 时 store 抬升 + `st.0 = max(st.0, base)` + info 日志（`"engine disk truth outranks the row — lifting the seed to absolute progress"`，可 grep 的命中痕迹）+ dirty 置位（抬升即落库）。`base ≤ seed`（合法 resume）走 max() 无操作路径，语义不变。
3. `rebase`/tripwire 读 seed 改 `load(Relaxed)`。

## 测试

新增（crates/scheduler/tests/scheduler.rs）：

- `readd_completed_target_reports_full_progress`（H1）：seed=0 + base=total + 完成帧 → 行落 received==total（修复前 0）。
- `readd_partial_target_reports_absolute_progress`（H2）：seed=0 + base=60k + 会话 40k（4 帧×300ms 隔离 drainer tick）→ 行落 100_000 绝对量（修复前 40k）；另附事件形态守护（R2 P1-1）：首个 publish 携带磁盘真相 60k（基线重置，非速度），此后每个 publish 增量 ≤ 整帧会话字节×2 —— 中途永不出现 60k 级跳变（那是修复前的幻影速度形态）。

不回归（原有）：`resumed_session_rebases_onto_row_reading`（seed=1000 > base=600 合法 resume，断言 6000 原样通过）、`duplicate_active_target_is_rejected` 等 24 项。

全量：**265/265 绿**（263+2）· clippy -D warnings 0 · fmt 净。

## 真机端到端（v2.0.0-alpha.1 debug 构建，tcp:8460）

| 步骤 | 结果 |
| --- | --- |
| R1 首次下载 aliyun ls-lR.gz → /tmp/p1-verify.bin | completed 38309351/38309351，磁盘 38MB ✓ |
| **重添加同 url+sink（bug 场景）** | **completed 38309351/38309351（修复前 0）**，daemon 日志 1 条 lift 命中 ✓ |
| partial 行重添加（H2 猜想路径） | **进不来**：`duplicate_active` 把 paused 行也挡住（"an active task … remove or finish it first"） |

边界结论：H2 的「重添加部分任务」实际暴露面为零——旧行必须 remove（段表 cascade）才能重加。单测保留 H2 覆盖作防御深度（若将来 duplicate 规则放宽，sink 侧坐标仍是对的）。

## 风险与自查

- 抬升只朝上（max），monotone 列不变式保持；total clamp 不变式在抬升分支保留（`min(t.max(base))`）。
- ABA/竞态：engine 报告面按契约先 `on_session_base` 后 `on_progress`；若某引擎乱序，`rebase` 的 max() 仍单调，最坏回到修复前行为（不会更糟）。
- 未动 auto.rs / segment.rs / storage——blast radius 限于 sink 一处 + 其构造。

R2 待办：delegate 独立复审本 diff（坐标系论证、Atomic 必要性、clamp 边界、测试断言强度）。

**R2 已回（结论：无 P0，同意提交）**：P1-1 速度声称无守护（已补事件形态断言 + 本文档口径修正：60k 磁盘真实体现在首个 publish，速度语义由「首帧基线重置」保证，而非「无跳变」）；P1-2 base 写回改单调（已修，防二次更小 base 通胀）；P2-1 clamp 注释与行为矛盾（已修注释）；P2-2 seed 锁序注释（已补）；P2-3 GUI 短暂超 100% 显示（触发苛刻，纯展示，backlog 不改）。详见 P1-readd-received0-R3.md。
