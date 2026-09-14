# Peregrine 🦅

> **游隼** — 俯冲时速 389 km/h 的地球最快动物，同时捕猎多个目标。

**Linux 首个「IDM 级加速 + Agent 原生」开源下载器。**

- ⚡ 自研 Rust 下载内核：IDM 式动态文件分段 + 自适应并发 + kill -9 断点续传，连接级遥测
- 🤖 MCP 一等公民：Claude / 任意 AI Agent 直接调度下载任务（10 工具 / 3 资源 / 事件推送）
- 🧩 headless daemon 架构：GUI / CLI / MCP 全是客户端，协议即插件

## v2.0.0-alpha.1 交付清单

| 能力 | 状态 |
|---|---|
| HTTP/HTTPS 多段下载 + 续传 + 限速 | ✅ M1 |
| 任务系统（队列/优先级/全局预算/WS 事件） | ✅ M2 |
| Web GUI + Tauri 2 桌面壳（托盘/单实例） | ✅ M3 |
| HLS VOD + live 录制 · FTP · BT/Magnet | ✅ M4 |
| MCP 服务器（stdio + streamable-HTTP） | ✅ M5 |
| 打包（tarball/deb/release CI） | ✅ M6 |

258 项测试 · clippy 零告警 · 每里程碑三轮审查（自查→独立复审→反思），
全部记录在 [docs/reviews/](docs/reviews/)。

## 快速开始

### CLI / GUI

```bash
# 启动 daemon（UDS 默认；--tcp 额外开 127.0.0.1:8800 供事件推送）
peregrined --tcp

# 下载
pg add https://example.com/big.iso -o ~/big.iso
pg list
pg limit <id> 2M        # 单任务限速（HTTP/HLS/FTP）
pg remove <id> --purge  # 删除任务并清数据（默认保留数据）
pg speed 10M            # 全局限速

# GUI（桌面壳另行构建）
```

### MCP

```bash
peregrined --tcp     # 1. daemon
peregrine-mcp        # 2. MCP 服务器（stdio；零参数即配对）
```

`peregrined --tcp` 监听 127.0.0.1:8800，`peregrine-mcp` 的事件桥默认连
`ws://127.0.0.1:8800/events`。socket 解析（PGRG_SOCKET → XDG_RUNTIME_DIR →
/tmp 回退）三端共用同一函数。

### 从源码构建

```bash
cargo build --release --locked        # 全部二进制
bash scripts/package.sh               # tarball + sha256（dist/）
bash scripts/package.sh deb           # 另加 .deb（需 cargo-deb）
```

## 协议支持

HTTP/HTTPS（多段+镜像降级）· FTP（被动模式+REST 续传）· HLS（VOD 全量 +
live 滑窗录制）· BitTorrent/Magnet（librqbit embed，同 hash 引用计数，
DHT 默认关、持久化关）

## 已知限制（v2.0.0-alpha.1）

- BT 任务暂不支持单任务限速（全局限速有效；librqbit ratelimits 在 BACKLOG）。
- MCP 订阅用 legacy `resources/subscribe`（Claude Desktop 当前方言）；
  `subscriptions/listen` 在 BACKLOG。
- `--http` 模式无鉴权，默认绑回环；不要暴露到非回环地址。
- 桌面包（AppImage/dmg）由 CI 构建，本地脚本只出 headless 产物。

里程碑计划见 [docs/design/PROPOSAL.md](docs/design/PROPOSAL.md)。

## 技术栈

Rust (tokio) 内核 + Tauri 2 + Svelte 5 桌面端 + rmcp (官方 MCP Rust SDK) + librqbit (BT)

---

*本项目与任何前作无关，是全新 clean-room 项目。License: MIT*
