# Peregrine 🦅

> **游隼** — 俯冲时速 389 km/h 的地球最快动物，同时捕猎多个目标。

**Linux 首个「IDM 级加速 + Agent 原生」开源下载器。**

- ⚡ 自研 Rust 下载内核：IDM 式动态文件分段 + 多源镜像 + 自适应并发，连接级遥测
- 🤖 MCP 一等公民：Claude / 任意 AI Agent 直接调度下载任务
- 🧩 headless daemon 架构：GUI / CLI / MCP 全是客户端，协议即插件

## 协议支持（规划）

HTTP/HTTPS · FTP · HLS/DASH · BitTorrent/Magnet

## 状态

🚧 **积极开发中** — 内核（分段/续传/取消/调度/限速）与 headless daemon（REST/WS）已落地，
Web GUI（任务列表/限速/分段遥测/完成通知）可用，Tauri 2 桌面壳（托盘/单实例/关窗后台）
已搭建（本仓无 GUI 系统库，桌面包由 CI 构建）。M4 协议扩展已交付 HTTP/HTTPS 分段 + FTP +
HLS（VOD + live 录制）；M5 MCP 服务器（10 工具/3 资源/事件推送，stdio + streamable-HTTP）
已可用。
里程碑计划见 [docs/design/PROPOSAL.md](docs/design/PROPOSAL.md)，
代码评审记录见 [docs/reviews/](docs/reviews/)。

### MCP 快速开始

```bash
# 1. 启动 daemon（--tcp 裸开 = 8800，事件推送零配置）
peregrined --tcp

# 2. 启动 MCP（stdio 模式；socket 默认解析与 CLI 完全同源）
peregrine-mcp
```

零参数即配对：`peregrined --tcp` 监听 127.0.0.1:8800，
`peregrine-mcp` 的事件桥默认连 `ws://127.0.0.1:8800/events`——
两边都不需要额外参数。daemon 侧 socket 解析（PGRG_SOCKET →
XDG_RUNTIME_DIR → /tmp 回退）与 CLI/MCP 共用同一函数，不会再漂移。

### MCP 服务器已知限制（M5）

- 订阅用的是 legacy `resources/subscribe`（Claude Desktop 当前实际所说方言）；
  2026-07-28 协议的 `subscriptions/listen` 在 BACKLOG。
- `settings://` 资源只读无推送（daemon 的 settings 写不走事件总线）。
- stdio / `--http` 模式无鉴权：`--http` 默认绑定 127.0.0.1，不要暴露到非回环地址。
- 资源列表里 `task://{id}` 随任务累积（已删除任务的 URI 不再出现在列表，但客户端
  缓存的 URI 会收到 RESOURCE_NOT_FOUND —— 重新读 `tasks://` 即可）。

## 技术栈

Rust (tokio) 内核 + Tauri 2 + Svelte 5 桌面端 + rmcp (官方 MCP Rust SDK) + librqbit (BT)

---

*本项目与任何前作无关，是全新 clean-room 项目。License: MIT*
