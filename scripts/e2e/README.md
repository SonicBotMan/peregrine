# scripts/e2e — 真网络端到端脚本（手动执行，不进 CI）

来源：QA-E2E R1 轮（docs/reviews/QA-E2E-R1.md）。覆盖 HTTP 分段 / FTP / HLS VOD+live / BT 的
真实网络下载、kill -9 崩溃恢复与完整性校验（md5/sha256/TS 结构）。

## 前置

```bash
python3 -m pip install --user --break-system-packages pyftpdlib   # ftp.sh 需要
cargo build --release --locked --workspace                        # target/release/{pg,peregrined}
```

## 用法

```bash
scripts/e2e/ftp.sh          # 本地 pyftpdlib：全量 + kill -9 REST 恢复（md5 双证）
scripts/e2e/hls.sh          # Apple VOD 双跑 md5 一致 + TS 结构 + live 录制（Bug1 红灯守卫）
scripts/e2e/http-kill9.sh   # 公网镜像：限速→kill -9→恢复单调性→md5 vs curl 参考
scripts/e2e/bt-debian.sh    # Debian 官方 torrent 全量，sha256 对官方 SHA256SUMS
```

环境变量：`QA_DIR`（默认 /tmp/pg-e2e）、`SOCKET`（默认 tcp:8500）、`PG_BIN`/`DAEMON_BIN`。

## 注意

- 公网脚本（hls/http/bt）依赖外部源可达性与速率，波动属正常；失败先换镜像/重跑再定性。
- `hls.sh` Part 2 自 Bug 1 修复（salvage_merge）后为正常回归守卫：取消必须产出 finalized 文件。
- 脚本自带 daemon/服务器生命周期管理；重复运行会复用 `QA_DIR` 下的产物。
