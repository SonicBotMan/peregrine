# UI U3 验证记录（精修与键盘层）

对应 ui-proposal.md §7（U3）；commit f56fd0d。真机验证 16/16（探针 1 项断言书写错误，产品行为全对——见下）。

## 交付

- **⌘K 命令面板**（CommandPalette.svelte，bits-ui Command 全自定义皮肤）：Actions 组（New download/Pause all/Resume all/Toggle theme）+ Tasks 组（全部任务，value 含文件名/URL/save_path/状态可搜）；↑↓/Enter 导航、模糊评分内置（compute-command-score）；选任务关面板→清过滤器→选行展开详情。
- **快捷键层**（App.svelte:window）：⌘K/⌘N//、?、Space（暂停/恢复选中）、Del/⌫（移除选中）；isTyping 守卫（input/textarea/select/contenteditable 内不抢键，⌘ 组合除外）；浮层打开时全局层让位（浮层自持 Esc）。
- **乐观移除 + undo**：Remove 即隐行 → 5s 后才真删（daemon remove）；undo toast 撤销（清 timer+行回归，数据未动）。visible 过滤 hidden 集（$state 数组）。
- **Toast 通道**（toast.svelte.ts + Toasts.svelte，B39 规范化）：模块单例 runes 状态；完成页内 toast + OS 通知双轨（notifyCompleted 保留）；动作反馈 toast（Pause all 计数）。
- **动效**：--dur 200ms / --ease token；行/hover/进度线 transition；SegmentPanel 展开、palette、sheet、toast 入场动画。
- Toolbar ⌘K 搜索框入口；? 速查表（ShortcutsDialog.svelte）。
- **Tauri 焦点风险消解**：快捷键全部页面级（webview 内 keydown 与浏览器一致），不依赖 OS 全局快捷键 API——提案风险项不成立，无需 plugin-global-shortcut。

## 验证矩阵（全绿）

| 项 | 结果 |
| --- | --- |
| 真机 U3 探针（palette 任务选择/Space 暂停恢复/Del+undo/? 表/⌘K 动作// / 开关/--dur/无页面错误） | 16/16 |
| vitest（store 15 + reactivity 2 + toast 3） | 20/20 |
| svelte-check | 0 error / 5 warning |
| vite build | ✓ |

探针唯一 FAIL 项：断言 palette 高亮 item 文本含 save_path——item 只显示文件名，行为正确（后续 selId===add.id 断言三连过）。已记录避免复发。

## U3 期间发现并修复

1. **Toolbar edit 撕裂 CSS**（edit oldText 只匹配块头，残留原块体 → vite dev 500 `Expected a valid CSS identifier`）：教训——多行块编辑必须含完整块或用锚点核对；svelte-check/build 不跑 dev server 编译路径，dev-only 编译错误须探针兜底。
2. **palette item value 漏 save_path**（探针暴露的真产品缺陷）：按路径搜索是合理场景，value 现含 save_path。

## 遗留

- R2 独立评审按 ui-proposal §4 逐条核对（sharp edges/色彩克制/边框层级/动效时长）→ 之后转 v1.0。
- 可选精修：palette 分组折叠、行内 hover 快捷键提示 tooltip（本轮以 title 属性代替）。
