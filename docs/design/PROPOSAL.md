# Peregrine（游隼）技术方案 — 全新独立项目

> 版本: v2.0-draft · 日期: 2026-09-12 · 状态: 待评审
> 定位: **全新项目（clean-room）**，与 Motrix AI v1 无任何代码/品牌/资产继承；v1 仅作为教训输入（§1.3、§7）
> 前提: Linux 桌面优先 · 对标 IDM 的多协议加速下载内核 · MCP/Agent 调度为一等公民
> 命名: 项目代号 **Peregrine（游隼）**——俯冲时速 389 km/h 的地球最快动物，同时捕猎多目标（多连接意象）；GitHub 下载器领域零冲突。发布前改名零成本，最终品牌名可再定

---

## 1. 调研结论（为什么这么设计）

### 1.1 竞品格局（GitHub 一手数据，2026-09-12 采集）

| 项目 | Stars | 技术栈 | 状态 | 核心架构 |
| ------ | ------- | -------- | ------ | ---------- |
| Motrix | 55.4K | Electron + aria2 | **已死**（~3 年无更新） | aria2 子进程 + JSON-RPC |
| aria2 | 42.1K | C++ | 半维护（年更级别） | 单体 CLI/RPC 引擎 |
| Gopeed | 26.3K | Go + Flutter | 活跃（日更级） | **引擎/前端分离**，REST over Unix socket，FetcherManager 插件协议层，JS 扩展系统 |
| AB Download Manager | 17.9K | Kotlin + Compose | 活跃（日更级） | DownloadSystem 编排器 + DownloaderRegistry 协议注册 + PartListDb 分段管理 + 桌面/Android 共享内核 |
| XDM 2018 | 7.9K | C#/Java | **停滞**（2024-01 后无提交） | 最接近 IDM 的开源实现，已死 |
| Persepolis | 7.5K | Python + aria2 | 缓慢 | aria2 前端 |
| FluxDown | 3.0K | Rust | 新锐（增长快） | 多协议 + 浏览器集成，Rust 新一代 |
| Surge | 3.5K | Go | 活跃 | TUI，Power-user 向 |
| Varia | 1.9K | Python GTK4 + aria2 | 活跃 | GNOME 原生壳 + aria2 |

**格局判断：**

1. 老一代 Electron+aria2 壳（Motrix/Persepolis/Varia）集体衰退——**壳不是壁垒，引擎才是**。
2. 活着且增长的（Gopeed/ABDM/FluxDown）全部是**自研引擎或深度定制分段**，且采用**引擎与前端分离的 client-server 架构**。
3. `xdm` 死后，**Linux 平台至今没有 IDM 平替**；Reddit（r/linuxquestions 等）持续出现"IDM for Linux"求荐帖，需求真实且未被满足。
4. 没有任何下载器原生支持 MCP——已出现第三方给 JDownloader 包 MCP wrapper 的项目，证明"Agent 驱动下载"是新兴真实需求，**先占位者得品类定义权**。

### 1.2 IDM 能力拆解（对标基准）

IDM 的核心竞争力按重要性排序：

| 能力 | 本质 | v2 对应 |
| ------ | ------ | --------- |
| **动态文件分段加速**（旗舰卖点，官方宣称 5x） | 把文件切成多段并行下载；先完成的连接**动态认领最慢段的尾部**，而不是固定分段等最慢的 | 自研 engine-http 核心 IP，详见 §5 |
| 断点续传 | Range 请求 + 分段状态持久化 | 分段表落 SQLite，精确到字节恢复 |
| 浏览器接管 | 扩展 + Native Messaging 拦截下载点击 | 路线图 M6（v2.1），核心稳固后做 |
| 站点抓取 (Site Grabber) | 递归抓取网页资源批量下载 | **v2 砍掉**，后续作为插件 |
| 视频抓取 | 识别流媒体并下载 | HLS/DASH 分段下载覆盖大部分场景（M4） |
| 队列/调度 | 优先级队列 + 定时 + 限速 | task-manager 一等能力（M2） |
| 多协议 | HTTP/HTTPS/FTP/MMS/RTSP | HTTP(S)/FTP/HLS/BT+magnet，见 §4 |

### 1.3 v1 的死因（必须避开的坑）

1. **三套类型系统**（Rust 后端 / TS core 包 / Vue 前端各自定义类型且默认值不一致）→ v2 铁律：**类型只定义一次**（api crate）。
2. **AI 硬编码进应用**（intent-parser 等规则玩具）→ v2 反转：**应用不做 AI，应用是 Agent 最好的工具**（MCP-first），AI 在生态里而不是二进制里。
3. **功能清单爆炸全是半成品**（字幕/调度/NAS/i18n×5/无障碍全做了，全没做好）→ v2 有明确的**冻结清单**（§7）。
4. **安全裸奔**（aria2 RPC 无认证绑 0.0.0.0）→ v2 默认 Unix socket + token，绝不监听外部接口。
5. **引擎不可控**（aria2 是黑盒子进程，无法做深度加速定制和连接级遥测）→ v2 自研 HTTP 引擎。

---

## 2. 产品定位

### 一句话

**Linux 平台第一款「IDM 级加速 + Agent 原生（MCP）」的开源下载器。**

### 三个差异化支柱

1. **极速内核**：自研 Rust 下载引擎，IDM 式动态分段 + 多源镜像 + 自适应并发，连接级遥测可视化（竞品都没有"看见每个连接"的能力）。
2. **Agent 原生**：MCP 一等公民。Claude Desktop / 任意 Agent 可以 `add_task / pause / schedule / query`——下载器成为 Agent 的"手脚"。
3. **优雅可扩展**：协议 = trait，前端 = 客户端，一切皆插件点。核心是 headless daemon，GUI/CLI/MCP/未来的 Web UI 全是它的客户端。

### 目标用户

- 从 Windows 迁移到 Linux、想念 IDM 的用户（基本盘，Reddit 已验证）
- Agent/自动化玩家：想让 AI 管理下载的重度用户（差异化增量）
- 开发者：要一个可靠的 headless 下载服务（CLI + daemon 模式白送）

---

## 3. 总体架构

### 3.1 架构原则（血泪换来的）

1. **Core 与壳彻底分离**：core 是 headless daemon，Tauri GUI 只是一个客户端（学 Gopeed，避免 v1 三层类型灾难）。
2. **协议即 trait**：`ProtocolEngine` trait，新协议 = 新 crate + 注册，零侵入。
3. **单一事实源**：所有类型（Task/Event/Config）只在 `api` crate 定义一次，GUI/CLI/MCP 直接复用。
4. **安全默认**：Unix socket + 本地 token，永不监听 0.0.0.0（v1 P0 的教训）。
5. **事件驱动**：tokio broadcast 事件总线，UI/MCP 订阅同一事件流，无轮询。

### 3.2 架构图

```
┌────────────────────────────────────────────────────────────┐
│                  peregrine-daemon (headless)                │
│  ┌──────────────────────────────────────────────────────┐  │
│  │                crates/api（类型 + 事件总线）           │  │
│  │   Task · Segment · EngineEvent · Config · trait 定义  │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌────────────┐ ┌────────────┐ ┌────────────────────────┐  │
│  │task-manager│ │  storage   │ │      scheduler         │  │
│  │ 状态机/队列 │ │ SQLite WAL │ │ 并发/限速/定时/重试      │  │
│  └────────────┘ └────────────┘ └────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │           engine-registry（ProtocolEngine trait）      │  │
│  ├──────────────┬───────────────┬───────────────────────┤  │
│  │ engine-http  │  engine-bt    │  engine-hls           │  │
│  │ 动态分段加速  │  librqbit封装  │  m3u8/DASH 分段        │  │
│  │ (旗舰核心IP)  │  magnet/种子   │  (复用 engine-http)    │  │
│  └──────────────┴───────────────┴───────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  transport: Unix socket (REST+WS) · 可选 localhost TCP │  │
│  └──────────────────────────────────────────────────────┘  │
└──────────────┬──────────────────┬─────────────────┬────────┘
               │                  │                 │
        ┌──────┴──────┐    ┌──────┴──────┐   ┌──────┴──────────┐
        │ apps/desktop│    │ crates/cli  │   │  crates/mcp     │
        │ Tauri 2 壳   │    │pg-cli       │   │ rmcp 官方SDK     │
        │ Vue 3 客户端 │    │             │   │ stdio + HTTP 双通道│
        └─────────────┘    └─────────────┘   └─────────────────┘
                                                   │
                                              Claude / 任意 Agent
```

### 3.3 Workspace 布局（Cargo workspace 单语言单类型）

```
motrix-ai/
├── crates/
│   ├── api/            # 类型 + trait + 事件总线（唯一事实源，零依赖 壳/引擎）
│   ├── engine-http/    # HTTP(S)/FTP + 动态分段加速器（旗舰）
│   ├── engine-bt/      # librqbit 封装：BT/magnet/做种
│   ├── engine-hls/     # HLS/DASH：m3u8 解析 + 分段调度（复用 engine-http worker）
│   ├── task-manager/   # 任务状态机 + 队列 + 生命周期
│   ├── scheduler/      # 全局并发、限速、定时规则、失败重试策略
│   ├── storage/        # SQLite (rusqlite + WAL)：任务/分段表/配置
│   ├── server/         # daemon：axum Unix socket + WS 事件流 + token 认证
│   ├── mcp/            # rmcp：tools/resources 映射到 api
│   └── cli/            # clap 薄客户端
├── apps/
│   └── desktop/        # Tauri 2：窗口/托盘/通知 + Vue 3 前端（纯客户端）
├── docs/design/v2/
└── xtask/              # 构建/打包自动化 (deb/rpm/AppImage)
```

---

## 4. 技术选型矩阵

### 4.1 桌面壳

| 方案 | 结论 | 理由 |
| ------ | ------ | ------ |
| **Tauri 2** ✅ | **选定** | 内存比 Electron 低 71%、冷启动快 63%（实测数据）；Rust 侧与 core 同语言零 FFI 成本；UI 用 Web 技术开发效率高、易做出"优雅"。已知 Linux 缺陷（WebKitGTK+NVIDIA 渲染、Wayland 托盘）均有官方 workaround 且**我们不做媒体播放**，风险可控（§8） |
| GTK4/libadwaita | 备选 | 真·原生，但复杂表格/虚拟列表开发慢，"优雅"达成成本高 |
| Electron | ❌ | v1 死因之一，内存怪物 |
| Slint/Iced/egui | ❌ | 此类复杂桌面应用成熟度不足 |

### 4.2 前端

**Svelte 5 + TypeScript**：runes 细粒度响应天然契合 WS 实时推送；编译产物最小、webview 内存友好；TanStack Virtual 做任务表虚拟滚动；组件按 shadcn-svelte 风格自建（下载器 UI 面积小，不值得背重型组件库）。备选 Vue 3 + Naive UI（组件库更成熟，若想求稳可切换）。架构上前端只是薄客户端，更换成本被架构性锁死在低位。

### 4.3 下载引擎（本方案核心决策）

| 方案 | 结论 | 理由 |
| ------ | ------ | ------ |
| aria2 子进程（v1 路线） | ❌ | 黑盒：无法实现 IDM 式动态分段定制、无连接级遥测、RPC 边界摩擦、加速故事讲不成立 |
| **自研 HTTP/FTP + librqbit BT** ✅ | **选定** | HTTP 分段技术成熟（Range + 语义明确），自研成本可控且成为核心 IP；BT 协议复杂度不值得重写，librqbit（v9.0.1，纯 Rust，活跃）作为库嵌入，藏在我们 trait 后面可替换 |
| libcurl multi | 备选 | C 绑定味道重，遥测粒度不如自研 |

**协议优先级**：HTTP/HTTPS（M1）→ BT/magnet（M4）→ HLS/DASH（M4）→ FTP（M4，suppaftp）。ed2k 砍掉（衰落协议）。

### 4.4 关键库

| 用途 | 选择 | 成熟度依据 |
| ------ | ------ | ----------- |
| MCP SDK | **rmcp**（官方 Rust SDK） | v3.3.0，crates.io 2596 万下载，官方维护 |
| BT 引擎 | **librqbit** | v9.0.1，纯 Rust，可嵌入库形态 |
| HTTP 客户端 | **reqwest** + hyper | 事实标准，11818★ |
| FTP | suppaftp | v12.0.0，396 万下载 |
| 存储 | rusqlite (SQLite WAL) | 单文件零运维，分段表高频小事务 WAL 最合适 |
| daemon 框架 | axum + tokio | Unix socket + WS 一等支持 |
| 异步 | tokio 全家桶 | 事实标准 |

---

## 5. 旗舰机制：动态文件分段加速（对标 IDM）

### 5.1 算法设计

```
阶段1 探测:  HEAD 请求 → content-length / accept-ranges / ETag / 服务器并发策略
阶段2 规划:  K = clamp(自适应默认, 8, 32) 段; 段太小(阈值如 5MB)则合并
阶段3 执行:  K 个 worker 各持 Range 光标; 每连接独立吞吐统计
阶段4 动态再平衡(IDM 精髓):
      worker 完成 → 认领"最慢活跃段"的尾部 T(自适应 5~10MB)
      慢段 worker 光标回退到新边界, 尾部交给快 worker
      → 消除"固定分段等最慢"的木桶效应
阶段5 自适应: 滚动窗口吞吐 ≤ 单连接中位数 × 1.3 → 增开 worker(到上限)
             服务器持续 429/限速 → 降 worker + 退避, per-host 策略记忆
阶段6 收尾:  段合并校验(总长 + 可选 hash), 原子 rename 落盘
```

### 5.2 工程保障

- **分段表持久化**：每段 `range / bytes_done / etag` 落 SQLite，崩溃/暂停后字节级精确续传（含 ETag 变更检测 → 全部重验）。
- **稀疏预分配**：`fallocate` 预建目标文件，各 worker `pwrite` 定点写入，无 .part 拼接成本。
- **限速**：令牌桶，全局 + 单任务两级。
- **正确性验证**：property-based test（proptest）+ 故障注入（随机断连/慢速节点）+ 下载后可选 hash 校验。

### 5.3 连接级遥测（竞品没有的卖点）

每个连接的实时速度/对端/Range 进度暴露到 API 与 UI——"看见下载的每一个连接"，既是调试利器也是产品差异化。

---

## 6. MCP / Agent 集成设计

### 6.1 定位反转（v1 最大战略修正）

v1 把"AI"硬编码进应用（规则 intent-parser），做成了玩具。
v2：**应用提供世界上最好用的下载 MCP 工具集，AI 由生态提供**（Claude Desktop、Claude Code、任意 MCP 客户端）。下载器即 Agent 的"手脚"。

### 6.2 双通道接入

| 通道 | 场景 | 实现 |
|------|------|------|
| **stdio** | Claude Desktop 等 spawn 式客户端 | `peregrine mcp` 子命令：连接本机 daemon，把 MCP 协议桥接到 api |
| **Streamable HTTP** | 常驻 Agent / 远端编排 | daemon 直接暴露 `http://127.0.0.1:<port>/mcp`（token 认证） |

### 6.3 工具集（首发）

```text
Tools:
  add_task(urls[] | uri, category?, priority?, save_path?)  → 任务ID（两步式：先 add 返回预览，confirm 提交——v1 P0 教训）
  confirm_task(task_id)        # 显式确认入队，AI 输出不直接落盘
  list_tasks(status?, tag?)    / get_task(task_id, include_segments?)
  pause / resume / remove(task_id)
  schedule_task(task_id, at|cron, 条件: wifi?/空闲?)
  get_stats()                  # 全局速度/队列深度
  search_progress(query)       # 语义查询任务（本地元数据过滤）

Resources:
  peregrine://tasks         # 任务列表（实时）
  peregrine://task/{id}     # 单任务详情含分段遥测
  peregrine://config        # 配置只读视图

Notifications:
  task.completed / task.failed / task.added → 推送给 MCP 客户端
```

### 6.4 安全边界

MCP 写操作默认走 **confirm 两步式**（`add_task` 只创建 pending 任务，`confirm_task` 才真正入队）；可在设置中为受信 Agent 开启自动确认。绝不允许 MCP 触达 shell 或任意路径写（保存目录白名单）。

---

## 7. 需求范围与冻结清单

### v2.0 MVP（M1–M5，12 周内）

1. HTTP/HTTPS 下载 + 动态分段 + 断点续传 + 限速（旗舰，M1）
2. 任务队列/状态机/持久化/全局调度（M2）
3. Tauri GUI：任务表格（虚拟滚动）、连接遥测面板、添加/暂停/删除、托盘、通知（M2–M3）
4. CLI 全功能（随 M1 起持续）
5. BT/magnet + HLS（M4）
6. MCP server 双通道（M5）
7. 打包：deb / rpm / AppImage（M6）

### 明确砍掉/冻结（防止 v2 重蹈 v1 覆辙）

| 冻结项 | 理由与去向 |
| -------- | ----------- |
| 字幕匹配、文件刮削整理 | v1 半成品重灾区；未来 = MCP 工具/插件，不进内核 |
| 智能调度（时段/磁盘自适应） | 简单版定时+并发+限速足够；智能版 = Agent 通过 MCP 做 |
| 5 语言 i18n、无障碍 WCAG | 首发中文+英文，其余社区贡献 |
| 浏览器接管 | **v2.1** 路线图首位（IDM 对标必须补上），核心稳固后做 native messaging |
| NAS/WebDAV/rsync | 与核心无关，daemon 模式天然支持后续 headless 部署 |
| ed2k | 衰落协议 |
| 内置 AI 意图解析 | 被 MCP 生态取代（§6.1） |

---

## 8. 风险与对策

| 风险 | 等级 | 对策 |
| ------ | ------ | ------ |
| WebKitGTK + NVIDIA 渲染白屏/花屏 | 中 | Tauri 官方文档记录的 env workaround（`WEBKIT_DISABLE_DMABUF_RENDERER` 等）探测脚本 + 首启自检；UI 避免重 GPU 路径 |
| Wayland 托盘图标缺失 | 中 | libappindicator 路线 + X11 fallback；AppImage 打包验证 |
| 动态分段正确性（合并错误/竞态） | 高 | 分段表单点所有权、proptest、故障注入测试、落盘前长度+hash 双校验 |
| 部分服务器限制多连接/反爬 | 中 | per-host 并发策略记忆 + 429 退避 + 默认保守值（8 连接）可调 |
| librqbit API 变动 | 低 | 锁版本 + trait 隔离层 |
| 范围蔓延（v1 死因） | **高** | 本文档 §7 冻结清单为契约；新功能一律先 issue 评审进 v2.x |
| rmcp 协议版本演进 | 低 | 锁定 MCP spec 2025-06-18，跟进官方 SDK release |

---

## 9. 里程碑路线图（12 周）

| 里程碑 | 周 | 交付物 | 验收标准 |
| -------- | ---- | -------- | ---------- |
| **M0 骨架** | W1 | 全新独立仓库初始化：命名定稿、新 logo/吉祥物、workspace、CI（fmt/clippy/test/coverage）、daemon hello-world | 新仓库首个 commit + tag；`cargo run` 起 daemon，CLI ping 通 |
| **M1 HTTP 引擎 MVP** | W2–4 | engine-http：动态分段/续传/限速/遥测；storage 分段表；CLI add/status | 大文件多连接下载速度 ≥ aria2 基线；kill -9 后字节级续传成功 |
| **M2 任务系统** | W5–6 | task-manager 状态机、队列、scheduler、WS 事件流 | 断网/崩溃恢复后队列状态一致；GUI 只读仪表盘连上 daemon |
| **M3 GUI 完整** | W7–8 | 添加/暂停/删除/限速 UI、连接遥测面板、托盘、通知 | 日常可用（dogfood 标准） |
| **M4 协议扩展** | W9–10 | engine-bt（librqbit）、engine-hls、FTP | magnet 下载+做种正常；m3u8 合流成功率 > 95% |
| **M5 MCP** | W11 | rmcp 双通道、工具集+resources+通知、Claude Desktop 实测文档 | Claude Desktop 通过 MCP 完成一次完整下载调度 |
| **M6 发布** | W12 | deb/rpm/AppImage、官网 README、v2.0.0 tag | 三种包格式安装可用 |

**v2.1 候选**（发布后按反馈排序）：浏览器接管扩展、站点抓取、Windows/macOS 移植、Web UI。

---

## 10. 与 Motrix AI v1 的切割（零继承）

- **零继承**：不保留 v1 的品牌名、图标/吉祥物、代码、CI 配置、release 流程中的**任何一项**。本项目落在全新独立仓库，新 logo、新 README、新社区口径。
- **v1 仓库处置**：`SonicBotMan/motrix-ai` 冻结为历史归档（main 原样保留），不做任何 v2 开发；本文档迁出至新仓库后，删除其中的 v2 分支，保持归档纯净。
- **只带走教训**：§1.3 死因分析与 §7 冻结清单是 v1 对本项目唯一的输入——它们是结论，不是包袱。
- **产品叙事**：「Linux 首个 IDM 级加速 + Agent 原生（MCP）下载器」，独立成篇，不与任何前作绑定。
