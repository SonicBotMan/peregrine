# M5 R2 处置记录 — MCP 服务器

日期：2026-09-14 · 审查人：reviewer delegate（del_mu14lbxt_f3nj）· 结论：无 P0，3 P1 / 若干 P2

## 处置

| # | 级别 | 发现 | 处置 |
| --- | ------ | ------ | ------ |
| 1 | P1 | `tools.rs:171` list_downloads 疑似慢查询（core count 分母） | **实证误报**：`storage/src/downloads.rs:176` `list_downloads` 是单条 SQL（`WHERE (?1 IS NULL OR status = ?1) ORDER BY priority…`），且 `idx_downloads_status` 索引存在（downloads.rs:29）。无应用层 O(n) 扫描，SQLite 万行级毫秒返回。加时长断言只会引入 flaky 测试，不改。 |
| 2 | P1 | priority 枚举大小写敏感，模型传 "High"/"NORMAL" 会被 REST 400 | `tools.rs`：decode 为 String，`to_ascii_lowercase()` 归一后透传；非法值返回可读 tool error（含期望枚举）。新增测试 `add_download_priority_is_case_insensitive`（HIGH→high ✓ / urgent→error ✓）。REST 契约保持严格，宽容只发生在 MCP 边界。 |
| 3 | P1 | add_download 语义不透明（相对路径、重复添加） | 工具描述重写：绝对路径要求 + 相对路径后果（daemon cwd 解析）+ 明示 no-dedup（同 URL 两次添加 = 两个独立任务，用 remove_download 清理）。 |
| 4 | P2 | RES_TASKS_URI 定义在 events.rs，lib.rs 用字面量（漂移风险） | lib.rs 两处改用常量。 |
| 5 | P2 | 无已知限制文档 | README 增「MCP 服务器已知限制」节：legacy `resources/subscribe`、`settings://` 只读无推送、无鉴权（--http 默认 127.0.0.1，勿暴露非回环）、已删任务 URI → RESOURCE_NOT_FOUND 的客户端行为。 |

## 记入 BACKLOG（不修，理由）

- `subscriptions/listen`（新协议方言）——当前客户端生态（Claude Desktop）实际说 legacy 方言，先对齐现实。
- `settings://` 变更推送——需要 daemon settings 写入走事件总线，属 daemon 侧改动，且 settings 变更频率极低。
- WS 长连接服务器侧会话累积 / task:// 死 URI 累积——量级（百级任务）下无实害，README 已写客户端应对。

## 验证

- `cargo test --workspace`：**242/242 绿**（新增 case-insensitive 测试）
- `cargo clippy --workspace --all-targets -- -D warnings`：0
- `cargo fmt --all`：干净
