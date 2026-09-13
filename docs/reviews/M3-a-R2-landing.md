# M3-a R2 迟到落地 — R3 处置记录

背景：M3-a 的 R2 delegate 报告延迟到达（del_mtzv7iom_eyae，基线 3ff0eff）。重新逐条
实证（grep 当前 HEAD 97d7d39）后分拣：P0-1/P1-2/P1-3/P1-4/P1-5/P2-6/7/9/10 在当前
代码上仍然成立（后续里程碑未触碰这些面），全部落地。P2-8 部分成立（已按观察式修）。

## 落地清单

**前端（P0-1 wire 镜像 / P1-2 时间戳 / P1-3 节流 / P1-4 onDown / P2-6/7）**

- types.ts：EngineEvent 完整镜像 bus.rs（8 变体 + 合成 resync_required）；
  created_at/updated_at 改 number（epoch 秒）；TaskStatus 删幽灵 'removed'。
- store.svelte.ts：内部时间全 epoch 秒（nowSec()）；applyEvent 9 case
  （+task_started/completed/failed{reason}/status_changed/resync_required）
  - default→resync（未来变体安全降级）；resync 节流（in-flight + 500ms 窗口 +
  trailing 定时器——中间发现并修掉「pending 无人观察」的死点）；EMA dt>5s 清零
  （stall 不冻结旧速度）；排序 tie-break by id。
- daemon.ts：EventStream 第 4 参 onDown（WS 死亡即刻翻 conn 徽章）；重连退避加抖动。
- App.svelte：connDown 回调接线；active 过滤去 'removed'。
- tests：fakeTask 改 epoch 秒；新增 7 个 wire 帧测试（completed 移出 active、
  failed 折叠 wire reason、started、resync_required、未知变体降级、stall 清零、
  未知 id 风暴合并为 ≤3 次 list）——15/15 绿。

**Rust（P1-5 / P2-8 / P2-9 / P2-10）**

- api.rs：`with_host_guard(router)` — TCP-only Host 校验（127.0.0.1/localhost/[::1]），
  防 DNS rebinding 打 POST /tasks；缺失 Host 也拒。3 个集成测试 + 真机 smoke：
  evil.attacker.example GET/POST 均 403，loopback/UDS 200。
- main.rs：TCP serve 错误路径不再跳过 drain/socket 清理；dual-mode 下 UDS 任务
  JoinError 观察并记 error 日志（abort 区分 cancelled）；unix-only 不再重复 await。
- cli.rs：tcp:0 拒绝（ephemeral 破坏 --print_socket 发现）；重复 kind 拒绝。
  2 个测试。

## 驳回/改判

- R2 P1-4（乐观置值不回滚）：M3-c1 复审已驳回（无乐观更新），维持。
- P2-8「select! 并发驱动」：改为观察式（UDDS panic 不应连坐 GUI 面），注释说明。

## 验证

cargo test --workspace 27 suites 全绿（含 server api 14）；clippy -D warnings 净；
vitest 15/15；vite build 绿；真机 smoke（上）。

## 教训（并入会话教训清单）

迟到的审查报告必须逐条对当前 HEAD 实证后再采纳——本报告 80% 条目仍活着是运气
（后续轮次恰好没碰这些面），但引用的行号/文件状态已全部漂移，直接照单全收会
引向不存在的代码。
