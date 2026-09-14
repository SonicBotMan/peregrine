# M6-c R2 独立审查处置（del_mu1es3kl_hcv3）

报告结论：**需修复**（2 P1 + 7 P2，无 P0）。核心代码（BoxedStream 委托、Endpoint 解析、authority/guard 处理、shell 枚举）经独立验证正确，263/263 复现一致。全部 9 项采纳处置：

## P1

- **P1-1 系统级模板单元以 root 运行**（无 `User=`，注释还声称"需要真实用户"）→ `User=peregrine`/`Group=peregrine` + `ExecStartPre=install -d -o peregrine /srv/peregrine-downloads`（连带解决 P2-5 无人建目录）+ README `useradd` 步骤。
- **P1-2 常驻 WS 连接使 `systemctl stop` 挂 90s 至 SIGKILL**（axum 0.8 graceful shutdown 等 upgraded 连接，而 ws 循环只退出于客户端 Close）→ 三层修复：①`Daemon` 暴露 `cancel: CancellationToken`，ws 循环 select 加 `shutdown.cancelled()` 分支（biased 首位）；②`shutdown_signal(token)` 收到 SIGINT/SIGTERM 后 **先 cancel 再返回**（两个 serve 循环各持 token clone，spawn 闭包外统一 clone 避免借用冲突）；③两单元 `TimeoutStopSec=15` 兜底。**实测：常驻 WS 下 SIGTERM → 102ms 退出**（原路径 ~90s）。

## P2（全部修复）

- **P2-1 端口范围零校验**（`tcp:0`/`tcp:99999`/非括号 IPv6 `tcp:::1:8899` 放行至连接期晦涩报错）→ 纯数字与 host:port 分支均按 `u16` 校验且拒 0；非括号 IPv6（≥2 冒号且非 `[` 开头）bail 提示用 `tcp:[::1]:PORT`；负例单测 ×5。
- **P2-2 `PGRG_SOCKET=tcp:8800` 被当 UDS 字面路径**（路由只看 `--socket`）→ 路由前置：`raw_spec = flag || env`，`tcp:` 前缀分流 `parse_checked`，其余才进 `socket_path()`。
- **P2-3 自定义拨号无 NODELAY + 错误消息失真** → TCP 分支 `set_nodelay(true)`；错误 context 按 `authority` 分支（"over tcp {a}" vs "over unix socket"）。
- **P2-4 MCP 未接 Endpoint（系统级 TCP 部署下 MCP 全灭）** → `PeregrineMcp::new(impl Into<Endpoint>)`，`--socket tcp:` 透传 `parse_checked`；**端到端验证：MCP-over-TCP initialize → tools/call get_settings → `{"global_limit_bps":0}` 真实 daemon 数据**。
- **P2-6 `--socket` 帮助文案不提 tcp 形态** → cli/mcp 两处帮助文本补全（completions/man 同步受益）。
- **P2-7 README 注释自重复** → 改为展示裸端口与显式 host 两种等价写法。

## 附带

- `Endpoint::display()`（日志用，tcp:authority / UDS path）。
- 删 `crates/mcp/src/lib.rs` 未用 `PathBuf` import。

## 验证

- `cargo test --workspace`：**263/263 绿**（新增 parse 负例 ×5 计入）
- `cargo clippy --workspace --all-targets`：0 warning；`cargo fmt`：干净
- TCP 端到端：`pg --socket tcp:18800 ping` / `tcp:127.0.0.1:18800` 双形态 ✓；常驻 WS + SIGTERM 102ms ✓；MCP-over-TCP 全链路 ✓
