# M4-a R3 — 终审反思

## R2（del_mu0492v0_cuy2，源码级验证 + RFC 引用核对）处置：全部 P1 落地

- **P1-1 ✅ 加密 init 静默损坏**：`MapSegment { uri, byterange, key }`——KEY 随 MAP
  走（RFC 8216 §4.3.2.4），merge 时 in-memory 解密（明文永不落盘）。新测试
  `encrypted_map_is_decrypted_at_merge`（init+seg 同 key 加密，断言解密后字节）。
  修测试时发现自己 fixture 又踩了「KEY 对后续段也生效」——段必须同样加密提供。
- **P1-2 ✅ 远程可触发 panic**：`expect("prev had range")` → BadPlaylist
  （"BYTERANGE continuation after a non-ranged segment"）。
- **P1-3 ✅ 无限挂起**：`tokio::time::timeout(30s)` 包 `body.frame()`（与
  engine-http STALL_TIMEOUT 同纪律）。connect 级超时 hyper legacy client 无内建
  per-request 机制 → B45。
- **P1-4 ✅ 空断言**：parts 断言按 `parts_dir` 语义（追加 `.parts`）计算——原断言
  检查的是错误路径 `out.parts`（实际 `out.ts.parts`）+ 恒真 stub。
- **P2-1 ✅** `is_hls_url()` 单一定义（engine 拥有；daemon RoutingPort 与
  `supports()` 均调用）。
- **P2-2/P2-8 ✅** `sweep_tmp()` 开局清扫兄弟 .tmp 残骸；`existing_parts_len`
  只计 `.ts`/`init.mp4`（.tmp 不算进度）。
- **P2-3 ✅** merge tmp 改追加 `.hls-merging` 后缀（`x.ts`/`x.mp4` 同目录不再
  碰撞）；merge 失败清理 tmp（parts 保留供 resume）。
- **P2-4 ✅** RoutingPort::purge `tokio::join!` 双侧都执行，错误合并。
- **P2-5 ✅** flush 失败清 .tmp。
- **P2-6 ✅** 删死码：write-only `AtomicU64` + 恒为 `&|_|{}` 的 progress 参数。
- **P2-7 部分 ✅**：BOM 剥离；MEDIA-SEQUENCE 必须在首段前（中段出现→报错，
  防 seq 重编号损坏）；both-master-and-media 错误消息。AttrIter 反斜杠转义
  剥离 → B46（真实世界 HLS 属性值几乎无转义引号）。
- **P2-9 ✅** R1 措辞修正：同栈同 builder 同 UA，但各 engine 各自 Client
  （连接池不共享）。`HlsEngine::probe` 生产不可达（daemon 不 probe，M5 MCP/CLI
  再接）→ B47。
- **P2-10 ✅** 记录已知限制：206 强制（对覆盖性 Range 答 200 的 CDN 会失败——
  正确性优先）；严格 PKCS7（拒绝不规范打包器）；supports 启发式假阳性代价=一次
  fetch + 干净报错。
- **R2 未采纳项（记 backlog）**：hls_port.rs 独立 port 级测试（cancel 映射/purge，
  daemon 集成测试间接覆盖）→ B48；SSRF 私网段守卫（与 engine-http 同信任模型，
  daemon 仅 loopback 时无行动必要）→ 记录于安全节。

## 验证

cargo test --workspace **214 全绿**（engine-hls 25：18 单元 + 7 集成）；
clippy --all-targets -D warnings 0；fmt 净。公网 smoke（修复前已验，修复不动
网络语义）：mux 502MB FHD 完整合流 + 22MB 断点续传。

## 本轮教训

1. **fixture 的 RFC 语义要对齐代码语义**：加密 init 测试第一版给明文 seg0 但
   KEY 仍生效——padding error 是 fixture 错不是代码错。写协议测试时先写
   「同一 KEY 覆盖谁」的清单再落 fixture。
2. R2 的「vacuous assertion」抓法值得复用：对 `a || b` 形断言逐边问「这个
   分支能假吗」——`out.parts` vs `out.ts.parts` 这种 append/replace 语义差
   是重灾区（parts_dir 自己就踩过 P2-3 同款）。
