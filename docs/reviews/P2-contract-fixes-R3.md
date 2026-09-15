# 契约修复轮 R3 — 复盘反思

日期：2026-09-15 · 前置：[R1](P2-contract-fixes-R1.md)（含 R2 复审结论节）· R2 报告 del_mu2bnvx7_rmjw

## 三轮结论

- **R2：同意提交，无 P0**；P1-1（etag 抹空 + 注释过声称）已修；P2×5 中 P2-3 已修、P2-1/2/4 记权衡、P2-5 不可达仅记录。
- revert-red 实证（R2 侧执行）：stash 修复 → `completion_keeps_plan_rows_for_telemetry_and_readd` 即红 → 判别力成立。
- 最终 268/268 绿 · clippy -D warnings 0 · fmt 净。

## R2 价值点（本轮学到的）

1. **全消费者穷尽排查**：R2 把 `get_task`/`delete_task` 全部 7 个生产调用点列成表逐条核验——R1 只论证了自己改的路径，「完成态无行」的隐式假设散布面靠自己 grep 一遍不保险。**教训：改「数据生命周期」类语义时，审查清单必须包含「谁还读这个数据」的穷尽枚举，不是抽查。**
2. **etag 防线名存实亡**：R1 注释写「changed etag replans too」作为保留行的安全论证之一，但生产重加链路 Route 1 在 probe 前短路 → validator 恒 None → etag 对比永不发生。R1 拿测试直传 validator 的可达性当生产可达性。与 P1 修复轮同型错误：**契约声称没有沿着真实调用链走一遍**。修复：`or(stored)` 保序 + 注释如实降级（two-ended 206 + total guard 是主防线）。
3. **意外收益被 R2 发现**：崩溃恢复（完成→tm.complete 前崩溃）旧行为 boot 全量重下，新行为秒完。修复的正向影响面自己没找全——「修复改了什么」之外还有「修复顺手治好了什么」。
4. **P2-1 权衡诚实记录**：保留行使「外部改文件→重加」从静默重下变为硬错误（M1-c1 有意设计的暴露面扩大）。R2 主动指出这与旧行为的差异并要求记录而非「修掉」——尊重原设计意图，不为了平滑改语义。

## 流程备注

- 本轮 server 层不加集成测试的论证（rig 是 mock port，engine 级覆盖核心语义）被 R2 接受，但 R2 把 daemon 全链路记为后续项（P2-4）——若 GUI 再现完成态段表问题，第一怀疑点是 Route 1×resume_job 组合而非段行本身。
- CLI unicode 宽度（P2-2）与 `clip_url(w=1)` 边界（P2-5）：均为「记录不修」——真实数据形态（ASCII URL、floor=8）使其不可达，为不可达分支引依赖/加复杂度是过度工程。
