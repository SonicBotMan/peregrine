# 轮E R3 反思（B36 遗留收口）

## 交付

- 行键漂移修复：降级重启的 validator 行以 original URL 键（`run_with_downgrade` 加 `original_url` 线程化）；测试 revert-red 复验——挂点在行键断言本身（auto.rs:624），非 mock 伪影。
- `valid_http_date`（RFC 7231 IMF-fixdate 定长 29 字节校验，手写零依赖）双侧应用于 `from_wire`/`from_probe`；垃圾 LM → None（validator-less resume）。
- BACKLOG B36 收口 RESOLVED + P2-4 WONTFIX（R2 同意论证）。

## R2 处置（del_mu2ox91w_3053：P1×1 / P2×1 / P3×2，修后同意提交）

| 项 | 级别 | 处置 |
| --- | --- | --- |
| P1-1 clippy 8/7 参（auto.rs:146）→ CI `-D warnings` 必红 | P1 | **已修**：`#[allow(clippy::too_many_arguments)]`（先例 run_download_impl） |
| P2-1 「纯安全方向」定性不成立（非规范自洽服务器角落：obs-date 精确串匹配下旧代码歪打正着提供变更检测，新代码回退到无 validator 盲续） | P2 | **定性修正已采纳**：from_wire doc 注释重写（"None 并不通向 safe replay，通向 validator-less resume；对非规范服务器是协议正确性 ↑ / 保护 ↓ 的交换"）；行为保留（R2 认可权衡） |
| P3-1 revert-red 经 mock 404 伪影 | P3 | **已修**：/real 裸 GET 改回 200；真 revert-red 复验挂点=行键断言 auto.rs:624 |
| P3-2 B36 BACKLOG 收口行被轮D 提交吞并（2dd2930 stat 含 docs/BACKLOG.md +5/-1） | P3 | 木已成舟、内容无恙；教训记录：多轮并行时提交前 `git status` 归属核对 |

## 教训

1. **clippy 输出被 `tail -3` 吃掉**：R1 声明「clippy 0」实际 1 warning——命令是 `clippy … | tail -3` 只见 Finished 行。验证命令的输出必须全量检查或 grep -c 计数（本次复验 `grep -cE '^warning|^error'` = 0）。R2 机器复跑 R1 声明是流程必要环节，本轮实证失效一次。
2. **「安全方向」定性要做反例扫描**：合规范服务器下等价 ≠ 全场景等价。非规范但自洽的服务器（自家 obs-date 串匹配）是真实存在的老 Java/PHP 形态。措辞从「safe」改成精确的交换描述。
3. **revert-red 的挂点必须是被测断言**：mock 用 404 兜底让「修复前必挂」兑现但挂因错误——回归信号指向性不足。一行 mock 改动 + 真复验即修复。
4. 并行轮的提交卫生：`git add` 按目录圈定也挡不住 BACKLOG 这类共享文件的跨轮吞并——共享文档要么单独提交，要么提交前 diff stat 核对。

## 状态

轮E 完成：clippy 0（全量复验）、workspace 292/292、fmt 过。五轮收尾计划全部完成（轮A f9008e8 / 轮B 29f1e2f / 轮C 72fa038 / 轮D 2dd2930 / 轮E 本次）。
