# 发布产物验证 R1 — alpha.2 tarball 缺 daemon（P1）+ 修复

日期：2026-09-15 · 触发：#33 发布产物真机验证（下载 GitHub Release 本体验证，非本机 build）

## 发现（按验证顺序）

1. **sha256 文件带 `dist/` 路径前缀**（P2）：`sha256sum -c` 在任意下载目录直接报 `No such file or directory`。哈希值本身匹配（b09106f7…）。
2. **tarball 只有 `pg`（P1，阻断）**：解包仅 `pg/README/LICENSE`。薄客户端没有 daemon 单独不可用——`scripts/package.sh` 的 `cp target/release/pg` 是唯一拷贝行，`peregrined`/`peregrine-mcp` 从未进包。cli/Cargo.toml 的 deb `assets` 同样只有 `pg`，且注释「daemon runs as `pg daemon`」为陈旧陈述（`pg daemon` 子命令不存在，daemon 是独立 crate 的 `peregrined`）。
3. **`peregrine-mcp --version` 不存在**（P3，记录）：意外，但 alpha 可接受（--help 正常）。

## 修复

- `scripts/package.sh`：拷贝三进制 + strip 三个；sha256 改为 `cd dist && sha256sum <bare>`（裸名，任意目录可 `-c`）。
- `crates/cli/Cargo.toml` deb assets：+`peregrined` +`peregrine-mcp`（package.sh 先 build workspace，三进制必在共享 target/release/）；注释改写为如实陈述。
- CHANGELOG：alpha.2 节内补「Packaging (re-release)」小节。
- 重发策略：删远端 tag+Release → 提交修复 → 同名 re-tag → CI 重建（版本号不动，Cargo.toml 无变化；对 alpha 阶段私有仓库无兼容性负担）。

## 验证（本地全链路，产物本体）

`bash scripts/package.sh` → tar 含 5 文件（3 二进制+README+LICENSE）✓ → 解包到干净目录 → `peregrined --listen tcp:8490` + `pg ping` ✓ → 真实下载 aliyun `ls-lR.gz`：**36.5MB / ~10s / completed**，`received_bytes == total_bytes == 38309960`（#30 修复在野生效），**md5 与参考一致**（0b19485d…）→ `pg list` 完整 21 字符 id（#31 修复在野生效）。daemon --help/--version、pg --help 均正常。

## 已知残留

- mcp 无 `--version`（P3，backlog：与 pg/peregrined 对齐 clap version 动作）。
- CI（ubuntu-22.04）产的 tar 与本机（本 dist）产物不同字节，sha256 以 CI 产物为准——Release 上传后需再抓一次校验（重发后执行）。

## R2 待办

delegate 复审：package.sh 改动的 shell 正确性（quoting/strip 失败容忍）、deb assets 相对路径在 cargo-deb 下的解析、re-tag 重发流程的风险（旧 Release 资产是否残留在 release 对象上）、CHANGELOG 措辞与事实一致性。

## R2 复审结论（del_mu2cycda_xrfe）——同意提交，全部实证

- **deb 路径实证通过**：R2 用本机 cargo-deb 3.8.0 实际打包+解包，三二进制齐全（`usr/bin/{pg,peregrined,peregrine-mcp}`），manifest-dir → workspace target dir fallback 生效；`extended-description-file` 同样取到仓库根 README。
- **cp/子 shell/strip**：`set -e` + 单条 cp 多源，任一缺失整脚本退出，不会静默残包；`(cd dist && sha256sum)` 裸名正确。
- **P1-1 重发策略改为 alpha.3（采纳）**：re-tag 需本地 `git tag -D` + 先删 Release 再删 tag，且同名 tag 指向不同 commit 是经典漂移坑。版本号 bump + CHANGELOG 新节，alpha.2 坏产物保留为历史。
- **P1-2 CHANGELOG systemd 失实（采纳，已修）**：`docs/systemd/` 不存在，实际为 `assets/peregrined.service` + `peregrined@.service`，无 `.socket` unit（旧文案把 UDS socket 与 systemd socket unit 混了）。
- **P2 采纳三项**：`depends = "$auto"`（deb 装 glibc 地板校验）；tar 可复现（--sort=name --owner=0 --group=0 --numeric-owner --mtime=@commit-time + gzip -n，实测两次构建字节一致）；desktop.yml 自 stage sidecar 不受 package.sh 影响（实证）。
- **P2 记 backlog**：deb 装 systemd unit + maintainer scripts、deb CI 覆盖（release.yml 只传 tarball）、mcp `--version`。
- **P3 记录**：package.sh 注释「zero external tools」vs strip 已顺手改；deb description synopsis 偏窄（不改）；dist 本地累积（不改）。
