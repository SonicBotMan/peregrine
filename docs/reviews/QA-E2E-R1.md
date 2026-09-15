# QA-E2E R1 — 协议引擎真网络 E2E + kill -9 恢复（执行记录与发现）

- 日期：2026-09-15
- 执行人：主 agent（用户授权的 QA 收口轮，todo #35）
- 二进制：target/release @ b060c28（v2.0.0-alpha.3）
- 环境：本机 x86_64-linux；daemon `--listen tcp:8500 --db /tmp/pg-qa/hls/db.sqlite`；无 docker，FTP 用 pyftpdlib（127.0.0.1:2121，被动端口 62000-62099，账号 qa/qapass）

## 执行矩阵

| 轨道 | 场景 | 结果 |
| --- | --- | --- |
| A-1 | HLS VOD：Apple bipbop advanced（真 CDN，598,645,016 B） | **PASS**：received==total；TS 结构校验（size%188==0；3,184,282 包，2102 采样同步字节全 0x47）；`.parts` 合并后清理 |
| A-2 | live：unified-streaming scte35（真滑窗流） | 引擎行为 **PASS**（250+ 段 seq 连续、重加幂等续录、跨 daemon 重启存活续录）；**Bug 1 (P0)** 见下 |
| B-1 | FTP 全量：30 MB / 300 MB / 1.2 GB 三档 | **PASS**：md5 逐位一致（a61bf161… / 485fcf59… / 30ddf96e…）；被动模式正常 |
| B-2 | FTP kill -9 @45%（582,746,112/1,288,490,188）→ 重启 | **PASS**：`crash recovery: re-queued interrupted tasks repaired=2`；`engine disk truth outranks the row — base=586940416 seed=582746112`（kill 时未刷库的 4 MB 由磁盘实况接管）；REST 续传完成；md5 一致 |
| C | BT：Debian 13.7 netinst 官方 torrent（公网 DHT/tracker，756 MB） | **PASS**：sha256 与官方 SHA256SUMS 一致（a7ef94ac…）；秒级完成（peer 充沛） |
| D-HTTP | Yandex 镜像 756 MB + `limit --bps 512k`（限速生效）→ t+6s kill -9 @751,793 B → 重启 | 恢复语义 **PASS**（received 8.8 MB→10.8 MB 单调续增，磁盘实况接管）；后续外源 stall 触发 `segment 0 stalled: no data for 30s`（引擎判定与报错精确，**正确行为**）；但「failed 后重加」暴露 **Bug 3 (P1)** |
| D-HLS | VOD 重下 @77 MB kill -9 → 重启 | **PASS**：续传完成 598,645,016 B，与首下 md5 逐位一致（b4283204…） |
| D-BT | 真网 kill -9 | **未执行**：环网+peer 充沛无中断窗口；断点恢复已有 crate 级测试覆盖。记为已知缺口 |

## 发现清单

### Bug 1（P0）live 录制 cancel 不 finalize——live 功能在真实世界不可用

- 复现：add live URL（无 ENDLIST 滑窗流）→ 录制 50 s（356 段 55 MB）→ `pg remove` → `removed`，任务消失，但 `live.ts.parts/` 原样保留，**输出文件未合并产出**。
- CLI 文案 `remove (partial files are kept)` 与行为一致，但用户拿不到可用产物：唯一 finalize 路径是流自然 ENDLIST，公共 live 流永不出现 → 录了 = 白录。
- 修复方向：remove/cancel 路径上 live 引擎把已录段合并为最终产物（「停录并交付已录内容」）；pause 不合并（保留继续录）。文案同步。

### Bug 2（P1）`pg limit` 对非 HTTP 引擎静默无效

- 复现：FTP 300 MB 任务 `limit --bps 4m` → 5 s 内完成（>60 MB/s 实测），限速零效果。
- 根因：`crates/engine-ftp/src/` 全目录 grep 无任何 rate/bps 逻辑；限速仅实现于 engine-http（download.rs/lib.rs/auto.rs/segment.rs）。CLI 不拒绝、不警告、无文档标注 → 用户误以为生效。
- 修复方向（三选一，倾向 a）：a) FTP 读取路径加 token-bucket（沿用 engine-http 的限速原语）；b) CLI/daemon 对不支持限速的引擎返回明确错误；c) 文档标注。BT/HLS 的 limit 行为需同轮核查（BT 有自身 throttle 机制，HLS 共享 http client 待查）。

### Bug 3（P1）HTTP 分段 disk-truth 把主文件 sparse 逻辑长度当进度 → failed 任务重加必 416

- 完整证据链（真实环境）：
  1. 任务 A 下载 756 MB 中途外源 stall → failed，主文件为 sparse 预分配：`stat` 792,723,456 B 逻辑长度 vs `du` 317 MB 实占；
  2. 同 URL 同 `-o` 重加 → 引擎按 len() 判定「已下 792,723,456」→ 发 `Range: bytes=792723456-`（起点=文件尾）；
  3. 镜像回 `http 416`，任务 failed；任务行 `received_bytes=792723456`（100% 假进度）。
  4. 交叉验证排除外因：该镜像 Content-Length 同为 792,723,456；`curl -r 792723456-` → 416；`curl -r 317853299-` → 206（起点=实占即可续传）。
- 用户影响：任何 stall/中断失败的 HTTP 分段任务，重试必 416 failed——除非手删文件。属常见操作路径的必炸缺陷。
- 疑似回归源：分段引擎引入主文件预分配后，len() 语义从「实下字节」变成「分配大小」，disk-truth/resume 复用 len() 未随之修正。
- 修复方向：a) disk-truth 的进度真源改为 per-segment 实况（.parts 段文件或段位图），不得用主文件 len()；b) 416 兜底自愈：收到 416 视为本地进度失真 → 校验后从 0 重下（或明确报「远端内容变更，建议 purge 重下」），不允许以裸 416 终态失败。

### P2 / 观察项

- **P2-a** daemon 重启后任务序号归零（新任务再次分配 `-0000` 后缀）。全局唯一性仍由随机前缀保住，但顺序可读性退化、且「序号」不再是单调量。修复方向：boot 时扫描已有 id 取 max+1。
- **P2-b** BT 单文件种子 `-o` 指定文件名被 torrent 内文件名覆盖（`-o debian.iso` 产出 `debian-13.7.0-amd64-netinst.iso`）。多文件种子 `-o`=目录合理；单文件应尊重用户名或文档化。
- 观察（正确行为，无需修）：外源 stall 30 s 判定 + 错误信息含段号与字节进度，定位精确；HLS master 相对 URI 拼接 RFC 正确（Akamai 某 test 流自身 playlist 失配 404，非我方问题）；remove 后重启不复活 removed 任务；HLS live 跨 daemon 重启存活并继续追新段。

### 流程教训（agent 侧，非产品）

- `pkill -f` 同命令行含模式可匹配串即自杀（本轮踩 2 次）；`cmd &` 优先级会吞掉 `cd`（后台化整段，前台 cwd 不变，本轮踩 2 次）。QA 脚本已规避。
- pg 任务 id = `<每任务随机前缀>-<会话内序号>`，只有序号可预测；脚本必须动态取 id（本轮硬编码前缀失误 2 次）。

## 测试资产固化计划（随修复轮提交）

- `scripts/e2e/` 固化本轮可重复脚本（R1 后已落盘）：`common.sh`（共享库：动态 tid 解析/pidfile 后台管理/kill -9 生命周期）＋ `ftp.sh`（本地 pyftpdlib：全量 md5 + kill -9 REST 恢复）、`hls.sh`（VOD 双跑 md5+TS 结构；live 录制含 Bug1 revert-red 守卫）、`http-kill9.sh`（公网镜像限速+kill -9+恢复单调性+md5 vs curl 参考）、`bt-debian.sh`（Debian 官方 torrent，sha256 对官方 SHA256SUMS）（外网依赖标注，不进 CI；用法见 scripts/e2e/README.md）。
- `docs/reviews/QA-E2E-R1.md`（本文档）。

## 下一步

R2：派 reviewer 独立复审三个 bug 的定性、根因定位（重点 Bug 3：读 segment.rs/auto.rs 确认 len() 来源与预分配引入点）与修复方向；随后修复轮 + revert-red 回归 + R3。
