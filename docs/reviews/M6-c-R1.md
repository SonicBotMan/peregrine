# M6-c R1 自查（第 1 轮）

范围：completions/gen-man、TCP 客户端（Endpoint/BoxedStream/authority）、systemd 单元、README 安装节。

## 验证记录

- `cargo test --workspace`：**258+5=263 绿**（+5 为本轮新增 `Endpoint::parse` 测试）
- clippy 0 / fmt 干净
- `pg --socket tcp:8899 ping`（裸端口）、`tcp:127.0.0.1:8899`（显式）、UDS 默认回退错误信息均正确（daemon `--tcp 8899` 实测）
- `pg completions bash/zsh`、`pg gen-man`（man -l 渲染验证格式正确）
- systemd 最小单元真机验证：`active` + `pg ping` `"status": "ok"` + stop 清理干净

## 发现与处置

- **P1-1 `Endpoint::parse` 零测试覆盖 + 含糊解析** → 重写为 `parse_checked`（失败于 `tcp:` 空尾 / `tcp:host` 无端口 / `tcp:host:` 空端口）+ `parse_infallible`（`From<&str>` 兼容回退）+ 5 个单测；`pg` 主程序改用 `parse_checked`（错误即刻可见）。
- **P1-2 `ProtectHome=tmpfs` 与 `ExecStart` 在 home 下矛盾**（真机验证抓到：binary 不可见）→ `BindReadOnlyPaths=%h/.local/bin` 挂回只读。
- **P2-1 容器 user manager 拒绝 sandbox 指令组**（status=218/CAPABILITIES，seccomp 拒 capability drop）→ 保留真机全硬化 + 单元内 NOTE 注释 + README 无需改（指令为 systemd 标准）。已验证二分定位：plain 单元 active、最小单元（PrivateTmp）active + ping ok。
- **P2-2 write 工具静默回滚（第 4 次）**：assets/ 两个单元文件报写成功实未落盘 → bash heredoc 重写 + `wc -c` 验证。教训同 M4-b2：关键文件落盘后立即磁盘校验。
- man 页 `SYNOPSIS` 中 `--socket` 未显示值占位（clap man 生成器的已知风格）→ 接受，P3。

## R1 结论

M6-c 交付物功能完整、验证通过。P1×2 已修复，P2×2 已处置。进入 R2 独立审查。
