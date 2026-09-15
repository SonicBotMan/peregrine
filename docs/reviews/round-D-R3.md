# 轮D R3 反思（B39/B41/B42 + R2 采纳项）

## 交付

- B39：`act()` 失败 → banner（6s 自清/点击关闭/role=alert），pickGlobal/commitCustomGlobal 同接；R2 审计确认 act() 覆盖无缺口（AddDialog 自有 inline error、SegmentPanel 遥测自展示）。
- B41：sidecar stdout/stderr/pipe-error/terminated 全量落盘 `app_log_dir/peregrined.log`（R2 对照 Cargo.lock 验证 tauri 2.11.5/plugin-shell 2.3.6：按行投递不含 `\n`，writeln! 恰补一行；降级路径不阻塞启动）。
- B42：deb 依赖闭环校验 step（dpkg-deb -I + grep webkit2gtk + 空 glob 兜底）。
- R2 采纳（del_mu2oe88m_n0y4）：P2-a1 pipe Error 事件落盘；P2-b1 a11y ignore（规则名实测为 a11y_no_noninteractive_element_interactions）；P2-d1 test script + CI svelte-check/vitest gate。
- **R2 之外顺手修掉的两个既有类型错误**（svelte-check 首次进 CI 暴露）：notify.ts 用了 plugin-notification 1.x 的 `mod.send`（2.x 是 `sendNotification`）；SegmentPanel 从 types.ts 导入不存在的 `TaskView`（实际在 store.svelte.ts）。

## 教训

1. **「构建过」≠「类型过」**：vite build 不做类型检查，两个真实类型错误潜伏数轮——svelte-check 一旦进 gate 立刻现形。任何「无测试框架」断言必须先查 package.json/devDependencies（R1 声称无框架，实际 vitest 在且 15 测试存在，缺的只是 script 与 CI 接线）。
2. R2 的「实测复现」（用项目自己锁定的依赖版本编译探针片段）比文档推断可靠——a11y 警告的规则名与 R1 预想不同，照抄预想规则名 ignore 无效。
3. B41 的价值链：R1 只论证了「release 下 stdout 进 void」，R2 进一步验证了 daemon 侧 tracing_subscriber 默认写 stdout（前提成立）+ 行语义细节。每层的验证都让结论更硬。

## BACKLOG 新增

- B64：sidecar log 跨会话无界增长（append 无轮转；阈值轮转方案）。
- B65：banner 状态机抽 lib/banner.svelte.ts + fake-timer 单测；open_sidecar_log 抽纯 FS 函数 tempdir 单测。
- B66：desktop.yml 无 pipefail，dpkg-deb -f 自身失败时报误导文案（step 仍红，可容忍）。

## 状态

轮D 完成：svelte-check 0 error、vitest 15/15、vite build 过、workspace 292/292、clippy 0、fmt 过。剩余：轮E（R2 进行中）。
