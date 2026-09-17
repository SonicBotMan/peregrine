# UI 开发工具手册（探针 / 截图 / VLM 盲评 / 验证门）

> 2026-09-16 UI V2→V5 + R2 五缺口迭代中沉淀的工具链。新会话接手 UI 工作时从本文件起步，不必重新发明。

## 1. 运行环境（端口与进程）

| 服务 | 启动 | 说明 |
| --- | --- | --- |
| vite dev | `pnpm dev`（apps/desktop） | 用 5202 端口起（旧 5199–5201 若占用是被僵尸 vite 占的，`pkill -f vite` 后重启） |
| daemon | `setsid cargo run -p daemon -- --socket 0.0.0.0:8420 &` | REST API <http://127.0.0.1:8420（/tasks、POST> /tasks、PUT /tasks/{id}/limit、DELETE /tasks/{id}） |
| 文件源 | `python3 -m http.server 8931 --directory /tmp/dlserve` | 布景文件放这里（big.bin/r2.bin 等，用 `dd` 造） |

浏览器走 vite 的 `/api` proxy 直连 daemon，无需 Tauri 壳。

## 2. E2E 探针（scripts/ui/ 种子）

种子：`probe-p0.js`（零步添加全路径）、`probe-v5b.js`（表格行/排序/徽标）、`probe-spark.js`（速度图 + sparkline 布景）。

骨架（Playwright，无裸 CDP）：

```js
const { chromium } = require('playwright');
const rest = (method, path, body) => new Promise(/* http.request 到 127.0.0.1:8420 */);
(async () => {
  // ① 清场：DELETE 所有任务
  // ② 布景：POST /tasks 造 running/completed/failed 各态（failed 用 9999 端口）
  //    ⚠ save_path 已存在同名文件会被断点续传判"已完成"——每轮布景换文件名（q- 前缀）
  // ③ 打开 http://localhost:5202/，断言清单化：每项 console.log('PASS/FAIL xxx')，失败计数 exit 1
})();
```

运行（Playwright 需要 nspr 库路径，否则 chromium 起不来）：

```bash
LD_LIBRARY_PATH=$HOME/.local/lib/nspr/usr/lib/x86_64-linux-gnu node scripts/ui/probe-xxx.js
```

**布景速度**：限速任务 `PUT /tasks/{id}/limit {bps: 60000}` 让 running 态稳定可见；等 12s 让 sparkline 攒样本。

## 3. 截图

`deviceScaleFactor: 2`（Retina 清晰度），双主题各拍：`page.emulateMedia({ colorScheme: 'dark' | 'light' })`。README 用的 7 张在 `assets/gui-v2-*.png`（dark/segments/palette/light/quickadd 等），布景保持 3-4 任务多状态（running + completed + failed + 文档类型）。

## 4. VLM 盲评（MiniMax）

```bash
mmx vision describe /tmp/shot.png --prompt "<见下>" --auth api-key --config ~/.mmx/config.json
```

prompt 要点（否则 VLM 只描述不打分）：「你是苛刻的桌面软件 UI 评审…请从信息密度、层级、对齐、配色、专业感逐项批评，1-10 分，**必须以 `SCORE: n` 结尾**」。

用法：每轮改版截图盲评，分数+批评驱动下一轮。实证收益曲线 V2 6.0 → V3 8.0 → V3.1 8.5 → V5 9.4。主观的"感觉不对"交给 VLM 量化。

## 5. 提交前验证门（与 CI 逐字对齐！）

```bash
# 本地裸跑 svelte-check 是宽松 tsconfig 的假绿——必须带 CI 同款：
cd apps/desktop && pnpm exec svelte-check --tsconfig ./tsconfig.app.json   # 0 error
pnpm exec vitest run          # 20/20
pnpm build                    # vite build ✓
cd ../.. && cargo fmt --all   # CI fmt 门
cargo clippy --workspace --all-targets -- -D warnings   # 禁管道：`| tail` 吞退出码
cargo test --workspace        # 292+
```

坑集（详细见记忆库 pi 空间 "工具链四坑"）：write 工具幻影（写后 `wc -c` 验证落盘）；vite HMR 半态（编辑风暴后 0 行零报错 → `git stash push <f> && git stash pop` 强制全量重编译）；管道吞退出码（`set -o pipefail`）；本地绿≠CI 绿（命令与 .github/workflows/*.yml 逐字对齐）。

## 6. 架构速览（改哪找哪）

```
apps/desktop/src/
  App.svelte          # 骨架：store 构建、quick add、键盘层、RemoveDialog 接线
  lib/
    store.svelte.ts   # 唯一状态源：轮询/filters/sort/优先级乐观更新
    TaskRow.svelte    # 42px 表格行（类型 tile/进度/速度/ETA/H·N·L 徽标）
    DetailPanel.svelte # 右抽屉：Segments/Speed graph/Errors 三 tab
    CommandPalette.svelte  # ⌘K
    RemoveDialog.svelte    # 删除语义（文件删除默认不勾选）
    Titlebar/Menubar/Toolbar/StatusBar/AddDialog/EmptyState
  app.css             # Motrix/Element 色板 + design tokens（--dur/--ease）
```

Rust 侧：`crates/task-manager`（状态机/priority 持久化）、`crates/daemon`（REST + 事件 SSE）、`crates/scheduler`（优先级队列）。新增 EngineEvent variant 后**全 workspace 搜 match**（E0004 穷尽性会在 mcp/events.rs 这类远处炸）。
