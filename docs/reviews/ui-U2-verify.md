# UI U2 验证记录（任务行与详情）

对应 ui-proposal.md §7（U2）；commit 9302e1d。R1 自验全绿，含两个验证期抓出的真实缺陷（1 个 GUI 侧、1 个测试环境侧）。

## 交付

- TaskRow 重写：ContextMenu.Trigger 包裹 7 列网格（chevron/name/pct/size/speed/eta/chip），tabular-nums 数列、2px 底部状态进度线（accent/ok/warn/err，indeterminate 动画）、悬停动作簇、选中 inset 环、右键菜单（Pause/Resume·Retry / Copy URL / Show in folder(completed) / Remove）。
- SegmentPanel 内嵌选中行下展开：限速迁移至此（preset select + custom bps 输入）、facts 增 path 行与 error 行、1s 轮询分片遥测网格。
- AddDialog：initialUrl 预填（拖放链路）、记住上次目录（localStorage `peregrine-last-dir`）、URL 方案校验、Escape 关闭（svelte:window，随 {#if} 挂卸）。
- 拖放：全窗口 dragenter/leave 深度计数 + dropping 描边；drop 取 uri-list 首个非注释行预填 AddDialog。
- 双击完成项 → revealSaved/openSaved（@tauri-apps/plugin-opener），浏览器上下文降级 banner。
- 依赖：plugin-opener（Rust+JS+capabilities）、@lucide/svelte 替换 deprecated lucide-svelte。

## 缺陷 1（GUI，已修）：$state(new Map()) 不是深度响应

- 现象：首帧 REST 快照正确，之后 pct/speed/状态/删除全部冻结；daemon 侧一切正常。
- 根因：Svelte 5 `$.proxy()` 只代理 plain object/array（proxy.js 对非 plain 原型原样返回），Map 裸穿 → 每个 fold 突变零版本信号；resync 的整体替换走 source 赋值才触发（故首帧正确，极具迷惑性）。
- 修复：`SvelteMap`（svelte/reactivity），突变自带信号；resync 改 clear()+set() 原地折叠（裸重赋值不被追踪）。
- 回归：tests/reactivity.test.ts——happy-dom mount 真组件（TaskProbe）覆盖 App 的 `visible = $derived(store.list…)` 形态，断言 fold/动作/删除均重渲染。vitest 需 `resolve.conditions:['browser']`（否则 mount 命中 server exports 抛 lifecycle 错误）。

## 缺陷 2（测试环境，已修）：dlserve 挂死 HEAD → 全部任务降级单流

- 现象：所有 http 任务 engine tasks/segments 表全空（单流），SegmentPanel 永远 "single stream"。
- 根因：dlserve（测试服务器）对 HEAD 走 GET 同一 pump，空写 300MB 虚拟 body 后才 end()（约 100s）→ probe 链 deadline 超时 → 路由表 "probe fails entirely → single"。真实服务器（tele2）无此问题。
- 修复：HEAD 立即 200 + content-length + accept-ranges。修复后新 sink 任务 8 段并发、聚合 ≈8×3MB/s（顺带复验了多连接加速）。
- 附带发现（非 bug）：同 (url,sink) 的历史单流 partial 会使新任务走 resume→single 路由（auto.rs 路由表第 2 行，正确语义）。测试造任务须用全新 sink。

## 验证矩阵（全绿）

| 套件 | 结果 |
| --- | --- |
| DOM（verify-dom 24 项：列对齐/进度线/分段条/ctx/主题翻转/tab 导航/Enter 选择） | 24/24 |
| 行为（拖放预填 / ctx Pause→Paused / Resume→Queued / 双击 banner） | 5/5 |
| 实时性（pct 85→95%、限速 63.7KB/s 实时反映） | ✅ |
| vitest（store 15 + reactivity 2） | 17/17 |
| svelte-check | 0 error / 5 warning（AddDialog a11y autofocus 等已知项） |
| vite build | ✓ |

## 探针脚手架教训（写入本记录防复发）

1. **playwright headless 缺库**：libnspr4/libasound 系统无包，须 `LD_LIBRARY_PATH=/tmp/nss-pkg/...:/tmp/alsa-pkg/...`（deb 手工解包）。
2. **pkill -f 自杀**：pattern 匹配到 bash -c 自身命令行 → 整条命令静默死亡。用变量拼接（`P="serv""er.js"`）规避。
3. **同步读 Svelte DOM**：dispatchEvent 后 effect flush 在微任务——同步检查必假阴性，dispatch 后 sleep ≥300ms 再断言。
4. **locator 漂移**：状态翻转后重查 `.first()` 会命中另一行；用固定 data-id 锚（TaskRow 已加 data-id 属性）。
5. **vite 端口漂移**：旧实例占 5199–5201（IPv6-only 监听，127.0.0.1 探活 000），新实例落到 5202；探活用 `localhost`。

## 遗留 → U3

- Cmd+K 命令面板、快捷键全集（bits-ui Command / 全局快捷键 API 焦点冲突首日验证）、乐观更新 + undo toast、动效精修（200ms）、B39 toast 规范化。
