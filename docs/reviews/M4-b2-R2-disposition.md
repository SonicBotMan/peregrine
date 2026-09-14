# M4-b2 R2 独立审查处置 + R3 反思

R2（delegate del_mu16hld4_d8hb）：3 P1 + 5 P2 + 4 P3，独立复现 247/247 绿 + clippy 0。全部采纳，处置如下。

## P1

- **F1 DHT 默认持久化+公网 bootstrap**（写 `~/.cache/dht.json`、测试连公网、注释不实）→ `BtEngine::new()` DHT 开但 `persistence: None`；新增 `BtEngine::offline()`（DHT 关，测试全改用）；`librqbit` `DhtSessionConfig` 显式构造；注释修正。
- **F2 非 magnet 源 purge 基本无效** → engine 内建 **Registry**（`url→TorrentId` + per-id `{output_folder, urls}`）：add 成功即登记；purge 走 `detach(url)`——最后一个引用才 `session.delete(id, purge_files)`，非最后仅解除该 url。跨重启空注册表回退 magnet hash 推导 → 再回退 remove_sink。
- **F3 同 hash 双任务互毁 + 幸存者永卡** → 三重防护：①`register()` 同 torrent 异 folder → `ApiError::InvalidInput` 拒绝（数据静默落错目录 = sink 契约违约）；②poll loop 每_tick `session.get(Id)` 探测消失（VANISHED_GRACE_TICKS=10 ≈3s 宽限后报 Internal 退出，不再幽灵自旋）；③purge 只在**最后一个 url** 解除时才删条目/数据——兄弟任务互不摧毁。

## P2

- **F4 set_task_limit 对 BT 静默无效** → daemon.rs RoutingPort：BT 源 `tracing::warn`（响亮拒绝）不装样子接受；BACKLOG 记 librqbit `SessionOptions.ratelimits` 接入。
- **F5 .torrent 后缀匹配不含 query/fragment** → `is_bt_source` 改 `Url::path()` 判后缀；`add_source` file:// 走 `to_file_path()`（处理 authority）；测试补 `?passkey=`/`#frag`/假后缀反例。
- **F6 错误一律 Network** → `map_bt_error()`：os error/NotFound/Permission → Io，其余 Network。
- **F7 BT 引擎自身不装 TLS provider** → `session()` get_or_init 首行 `let _ = rustls::crypto::ring::default_provider().install_default();`（幂等、race-safe，库嵌入路径结构性封死）。
- **F8 purge 数据删除零覆盖** → 新测试 `purge_after_completion_removes_data_and_session_entry`（完成后 purge：数据真删+重加即全新会话）；`same_torrent_into_second_folder_is_rejected`（F3 回归）。

## P3

- **F9 R1 处置声明失真**（handle 泄漏误报记 P1 已修）→ R1 文档已勘误。
- **F10 once_cell_lite MSRV 理由不成立** → 删 30 行手写同步原语，换 `std::sync::LazyLock`。
- **F11 依赖卫生** → librqbit/rustls 入 `[workspace.dependencies]`；engine-bt 删未用 tracing。
- **F12 sink 非 UTF-8 有损转换** → 概率极低（daemon 验证路径），BACKLOG 备注。

## 验证

- `cargo test --workspace`：**249/249 绿**（engine-bt 7：路由 +F5 用例、metadata→cancel、离线完成、取消重加自愈、purge 幂等、+purge 数据删除、+folder 冲突拒绝）
- `cargo clippy --workspace --all-targets`：0 warning；`cargo fmt`：干净
- 测试全程 offline（DHT 关）——R2 独立复现时发现的「测试连公网」污染已消除

## R3 反思

1. **外部库默认值即隐式契约**：`DhtSessionConfig::default()` 悄悄持久化+连公网，注释还写着「No session persistence」——embed 库的每个 Default 都要逐字段审计，注释描述的是意图而非性质。
2. **purge 是多态动词**：HTTP 删 resume 行、FTP 删文件、BT 删「会话条目+可能共享的数据」。同 hash 多任务共享一个 session 条目使 purge 变成引用计数问题——registry-first 设计一次到位，比「按 hash 删 + 祈祷没有兄弟任务」稳。
3. **R1 自查的盲区恰是 R2 的主战场**：本轮 R2 抓的全是「跨组件交互」(路由后缀、限速转发、purge 链路)——单 crate 视角看不见。三轮审查里 R2 的价值密度最高，不可省。
4. **write 工具在本轮出现静默回滚**（报成功但磁盘未变）：发现后用 md5 校验+落盘后 `wc -l` 交叉验证。教训：关键文件写完立即校验磁盘状态，别信工具返回值。
