# 发布产物修复轮 R3 — 复盘反思

日期：2026-09-15 · 前置：[R1](P3-pkg-verify-R1.md)（含 R2 处置）· R2 报告 del_mu2cycda_xrfe

## 三轮结论

R1（产物验证发现 + 修复方案）→ R2（**实证级复审**：cargo-deb 真打包真解包、tag 状态实测、引用点穷举——同意提交 + P1×2/P2×4/P3×4）→ 全部采纳落地 → alpha.3。同轮附带交付 P2-4 全链路回归测试（269 绿，fmt/clippy 净）。

## 反思

1. **发布产物的验证永远要下载数字交付物本体**：R1 的发现（tar 缺 daemon）来自「从 GitHub Release 下载 tar → 解包 → ls」这三步——CI 绿灯、本地 `cargo build` 成功、tarball 构建脚本跑通，都不代表「用户拿到的东西能跑」。之前 alpha.1/alpha.2 两轮发布都跳过了这一步，坏包发了两次才在 #33 被抓住。**教训：Release 是产品，CI 只是工厂；出厂检验要开箱。**
2. **R2 的实证文化这轮价值最大**：deb assets 相对路径问题我 R1 只标了「待验证」，R2 直接装 cargo-deb 打包解包给出结论；「re-tag 还是 alpha.3」我倾向 re-tag（省版本号），R2 用「同名 tag 指向不同 commit 的漂移坑」+「GitHub Release 残留 draft 行为」两条实证风险翻转了决策。**教训：发布流程的坑在流程语义里不在代码里，靠实证列举而非直觉。**
3. **可复现构建一次到位**：第一版只加了 owner/group/gzip -n（R2 建议的最小集），自查时发现 mtime 仍漂移（两次构建哈希不同），补 `--mtime=@commit-time` 后实测字节一致。**教训：可复现性要写一个「连跑两次比对哈希」的验证步骤，不是加了 flags 就算完成。**
4. **P2-4 测试的断言字段错了两次**（`state` vs `pct`/`done`、`segments` 包装层）——都是 wire 形状假设没先看 api.rs 的事实。轻量教训：写断言前先 curl 一次真实响应（或读 wire 类型定义），别从记忆里写 JSON 形状。
5. 测试尺寸踩了 `min_segment_bytes=5MiB` 默认值——256KiB 体走单流无段行。这类「配置默认值 vs 测试假设」错配靠失败信息（空数组）快速定位，成本可控；顺带确认了 `[]` 对单流任务是**契约内**行为（daemon 注释写明）。

## Backlog 增量

- deb：装 systemd unit + postinst maintainer scripts（`assets/peregrined@.service` 进 `usr/lib/systemd/system/`）
- release.yml：deb 构建进 CI（当前只有本地实证）
- `peregrine-mcp --version`（与 pg/peregrined 对齐）
