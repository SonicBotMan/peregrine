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
Web GUI 与 Tauri 桌面端在 M3。里程碑计划见 [docs/design/PROPOSAL.md](docs/design/PROPOSAL.md)，
代码评审记录见 [docs/reviews/](docs/reviews/)。

## 技术栈

Rust (tokio) 内核 + Tauri 2 + Svelte 5 桌面端 + rmcp (官方 MCP Rust SDK) + librqbit (BT)

---

*本项目与任何前作无关，是全新 clean-room 项目。License: MIT*
