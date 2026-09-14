# M4-c R3 反思（FTP 引擎）

## 轮次定位

PROPOSAL W10 收口：V2 协议矩阵（HTTP 分段 / HLS VOD+live / FTP）全部落地。
R2 结论见 docs/reviews/M4-c-R2.md（或 delegate 报告），P0/P1 修复后提交。

## 设计决策（为什么这么做）

- **SIZE 是 resume 的唯一权威**：FTP 无 etag/If-Range，唯一能验证远端
  未变的信号是当前 SIZE ≥ resume offset。远端变短 = 文件被换过 =
  HTTP 语义里的 200-replay，必须 truncate 重下。**无 SIZE 服务器上
  拒绝盲 append**——把不可验证的续传变成静默损坏（坏文件比慢文件
  代价高）是本项目一贯红线（engine-http 同规则）。
- **probe 失败不是 probe 错误**：SIZE 缺失/失败 → content_length
  None，下载照跑（unknown-total 模式）。协议能力差异不该表现为
  API 拒绝。
- **短读=错误**：数据连接 EOF 但字节数 < SIZE 宣称 → "short body"
  错误而非静默完成。与 engine-http short-body 同判。
- **取消的 flush 契约**：cancel 时 flush 落盘 + 最后一次 progress
  上报 + 有序 QUIT——partial 文件保真（resume 才有意义），mock
  测试直接断言「partial 是 body 的严格前缀」。
- **port/engine 分层照抄 HlsAutoPort**：budget 组装在 port
  （local 无限 + global 接入），协议在 engine。per-task 限速是
  B44 统一 backlog，不为 FTP 单独发明机制。

## 流程教训

1. **集成测试要打真 TCP**：mock FTP 服务器完整实现命令序列
   （USER→TYPE→SIZE→REST→PASV→227→RETR→150/226 时序），抓到了
   suppaftp 真实行为的坑（into_split 半边类型、PASV 端口编码）。
   纯 trait mock 会漏掉协议层缺陷。
2. **同一轮内 clippy -D warnings 必须零**：本轮 redundant_closure
   和 collapsible_if 两次返工——写时就该按 clippy 风格写。
3. **测试 spec 构造先想清楚再写**：no_size 测试第一版用键名技巧
   绕（SIZE miss 但 RETR hit），写出来自己都看不懂；加 no_size
   显式开关一行解决。测试的可读性是测试的一部分。

## v1 边界（backlog 对齐）

- 单连接单流：FTP 分段（多控制连接 + REST 分片）v1 不做——
  服务器兼容性差（很多实现不支持并发 RETR 同一文件）。
- FTPS（AUTH TLS）不做：BACKLOG B46。
- 服务器 421/重连策略：suppaftp 不自动重连，传输中断 = 任务失败
  → scheduler 重试层兜底（M2 的 retry 语义）。
