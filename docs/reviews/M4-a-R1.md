# M4-a R1 — 作者自查

范围：engine-hls crate（HLS VOD 合流引擎）+ HlsAutoPort + daemon RoutingPort 接入。

## 交付物

1. **crates/engine-hls**（新 crate，22 测试）：
   - `playlist.rs`：RFC 8216 子集手写解析器——master（BANDWIDTH 选最高变体）、
     media（EXTINF/KEY/MAP/BYTERANGE/MEDIA-SEQUENCE/ENDLIST）、未知 tag 忽略（§4.3）、
     quoted-attr 迭代器（带转义）、相对 URI 解析、BYTERANGE 无 start 续接规则
     （同资源接前 range，异资源报错）、MEDIA-SEQUENCE 偏移段号。
   - `decrypt.rs`：AES-128-CBC（RustCrypto aes/cbc），PKCS7 由 cipher API 剥离
     （错 key = padding error，绝不产出垃圾字节）；缺省 IV = 段号 BE 16B（§5.2.1.1）。
   - `fetch.rs`：共享 hyper 栈（engine-http 暴露 `https_client()/HttpsClient/USER_AGENT`
     ——一个 HTTP 故事，不引第二套栈）；Range GET（206 强制，200 视为服务器忽略
     Range → 报错防合流损坏）；≤3 跳重定向（签名 CDN 302）；budget 分片写盘；
     cancel 删 .tmp 后返回。
   - `lib.rs`：`download_merge` 编排——master→变体→VOD 检查（live 明确 Unsupported
     →M4-b）→ MAP init → 密钥去重预取 → 段并发（默认 4，buffer_unordered）
     → 按播放列表序合流（AES 段内存解密）→ rename 落 sink → 清 parts。
     **幂等 resume = parts 目录**：present part 必完整（tmp+rename 原子），重跑跳过。
   - `ProtocolEngine` impl：supports（.m3u8 启发式）、probe（#EXTM3U 验证 +
     文件名 stem.ts）。
2. **scheduler/hls_port.rs**：`HlsAutoPort`（DownloadPort）——global budget 接入
   （per-task throttle B44 no-op）、purge=删 parts 目录、cancel 映射 ApiError::Cancelled。
3. **server/daemon.rs**：`RoutingPort`（.m3u8→HLS，其余→HTTP）；purge 双侧都调
   （URL 换路由后清旧引擎残留）；set_task_limit 转发双侧。

## 自查发现与修复（编码中已修）

- **F1（P0）**：decrypt 初版手写 unsafe 块循环 + 手剥 PKCS7，`aes 0.8` 无
  `UnsafeBlockSize` API；改 `decrypt_padded_mut::<Pkcs7>`（安全 API，padding 校验
  由 cipher 做）。修完发现还要 truncate——`decrypt_cbc` 曾只返回长度不截 buf，
  集成测试 152≠136 字节当场抓获。
- **F2（P1）**：并发下载初版用 iterator-adapter 链（filter+map+buffer_unordered），
  过 `Box<dyn Future>` 边界时触发 rustc `FnOnce is not general enough`（higher-ranked
  closure 生命周期）。改显式收集 `Vec<Pin<Box<dyn Future>>>`——克隆所有权，无借用
  生命周期可出错。
- **F3（P1）**：初版想引 reqwest——workspace 根本没有（engine-http 是自研 hyper 栈）。
  架构修正：engine-http 暴露 `https_client()`，HLS 复用同栈同 UA 同连接池。
- **F4（P2）**：测试 fixture 相对 URI 写错（`../init.mp4` 相对 playlist 是
  `/v/init.mp4`，路由在 `/v/hi/`）——404 抓获后改 fixture。RFC 语义（相对当前
  playlist URL 解析）是对的。
- **F5（P2）**：MAP 的 BYTERANGE 无 @start 在 RFC 里语义不完整（init 是首字节，
  无「前一 range」可接），v1 直接拒绝。

## 验证矩阵

- cargo test --workspace：**211 全绿**（engine-hls 22：18 单元 + 4 集成）；
  clippy --all-targets -D warnings 0。
- 集成（本地 axum origin）：master+AES+MAP 全链路合流字节级比对；取消中途
  （单并发+80ms 段延迟保证时序）→ 无 .tmp 泄漏 → 重跑续传完成；live 拒绝；
  probe 命名。
- 公网真机：mux test-streams master（502MB FHD 变体自动选中）完整合流，TS sync
  0x47 对齐 100/100 包；ld 变体 12s 杀 36 parts → 重跑幂等续传 22.4MB 合流成功、
  parts 清理。

## 已知限制（记录不隐瞒）

- live playlist 拒绝（M4-b）；SAMPLE-AES 拒绝；DASH 未做（PROPOSAL M4 范围是 HLS）。
- per-task throttle no-op（B44）；无段级遥测（B43，wire SegmentView 是字节区间形）。
- HLS 段级重试：v1 无单段重试（失败即任务失败，resume 幂等使重跑廉价）——
  scheduler 的任务级 retry 覆盖真实场景。
