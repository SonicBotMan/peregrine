# GUI 真机验证 R1 — 发现 P1：同目标重添加任务 completed 但 received=0

日期：2026-09-15 · 轮次：GUI 真机验收（v2.0.0-alpha.1 二进制）
验证环境：`peregrined --listen tcp:8420+unix:/tmp/peregrine-gui.sock --db /tmp/peregrine-gui.db` + vite dev(5199) + Playwright headless chromium

## 一、GUI 验证结论（正面，全部通过）

| 项 | 结果 |
| --- | --- |
| SPA 渲染（Svelte 5, served by vite 5199） | ✅ h1=Peregrine, 1.2s |
| 空态 | ✅ "Nothing in flight." |
| Add 对话框 → 提交真实任务 | ✅ tele2 20MB 进入 ACTIVE |
| 列表实时刷新 | ✅ 12.2→13.7→17.8MB 多次独立采样递增 |
| WS 事件流（独立通道验证） | ✅ 44 事件：added→started→33×progress(≈0.35s/条)→completed |
| 限速预设下拉 | ✅ off/512K/1M/2M/4M/custom 渲染正常 |
| 完成→COMPLETED 区迁移 | ✅ COMPLETED(n) 计数与行迁移正常 |
| 断线灯 | ✅ ● live / ◌ connecting / ✕ down 三态渲染 |

## 二、P1 Bug：同 已完成任务重添加 → completed + received_bytes=0

### 复现（确定性，无需外网）

```
POST /tasks {url:"http://mirrors.aliyun.com/ubuntu/ls-lR.gz", save_path:"/tmp/bus-probe.bin"}
# 该文件已完整存在于磁盘（前一同名任务下载完成）
# 结果：3s 内 status=completed, received_bytes=0, total=38309351
```

GUI 表现：COMPLETED 区出现 "0 B / 36.5 MB · 0%" 行（用户可见的自相矛盾）。

### 根因链（代码级）

1. `engine-http/src/auto.rs:50` Route 1：`store.get_task(&job.url, &job.sink)` 命中**旧 completed 任务行**（同 url+sink，B31 purge 只在显式 remove 时清段表，completed 行的段表残留）→ 走分段 resume。
2. `engine-http/src/segment.rs:312` 起的 resume：文件 stat == total，段表全 complete → initial_done=total，零字节待写，直接合成完成。
3. `segment.rs:326-330`：`on_session_base(total)` + `on_progress(total)`。
4. `scheduler/src/lib.rs:983` `rebase`：`engine_value - base + seed` = `total - total + 0` = **0**。sink 的 seed 是**新任务行**的 received（0），而 base 是**旧任务行**的绝对字节——跨行复用段表 + 新行 sink 的 seed/base 失配。
5. `finish(resume_start, session_bytes=0)`：`rebase(0+0)` 或 monotone max 均停在 0 → 行 received=0 落库 + 完成。

### 为什么三层防护都没接住

- Route 1 只查「行存在 + total 已知」，不查「段表是否已全完成」；
- 416-settling 路径（download.rs:238）正确上报 on_progress(total)，但本路径根本不发请求；
- sink 的 monotone max 防倒退，防不了「合法 rebase 到 0」。

### 修复方案（待 R2 审后实施）

- **A（根本）**：scheduler `add()` 对同 检测旧行/段表 → 先走 B31 `purge()`（内部态清理，不删用户文件——文件已完整时 purge_files=false，磁盘 38MB 保留）再建新行。语义：重下=新会话，旧段表不复活（正是 B31 注释宣称的 invariant，add 路径漏了它）。
- **B（防御）**：Route 1 过滤全完成行：`existing.filter(|t| t.total.is_some() && !t.segments.iter().all(|s| s.is_complete()))`。
- **C（语义兜底）**：segmented resume 完成路径无条件补发 `on_progress(total)`（initial_done==0 且 ranges 为空时空转完成也要上报终值）。
- A+B+C 三层都上：A 修本路径，B 修同族（stale 全完成段表），C 保证任何空转会话的终值上报。

### 回归测试（修复后必须）

1. 同 url+sink 三连提交（完成→重加→再重加）：三轮均 completed 且 received==total；
2. 中断后重加（部分段表未全完成）：resume 正常继续，received 单调；
3. 既有 GUI 冒烟：新页面添加新任务实时进度不受影响。

## 三、过程中的次生发现（P3，记录不修）

- vite dev 监听 `[::1]`（IPv6-only）——curl 127.0.0.1 连不上 5199，页面 localhost 正常；仅影响本机调试姿势。
- E2E 脚本断言陷阱两枚（已绕过）：任务行显示 URL 尾段文件名而非 save_path basename；COMPLETED 计数 break 条件需按基线任务数偏移。
