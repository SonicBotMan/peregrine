# QA-E2E R3 — 收口反思（#35）

日期：2026-09-13 · 作者：主 agent（R3 自省轮）
对象：QA-E2E R1（docs/reviews/QA-E2E-R1.md）→ R2 复审 → 修复轮的完整闭环

## 1. 本轮闭环回顾

| 阶段 | 产出 | 结论 |
| --- | --- | --- |
| R1 执行 | 公网 e2e 实测（HTTP/FTP/HLS/BT × 正常+kill -9） | 抓到 Bug 1/2/3/4 + 3 条流程教训 |
| R2 复审 | del_mu2gwmk0_a221（读代码验证 R1 定性） | 3 个 bug 全部定性成立，采纳全部修复建议；Bug 4（FTP 21 端口）关闭为 by-design |
| 修复轮 | 3 个 fix + 34 个新测试 | workspace 229/229 绿，clippy 0 警告 |

## 2. 修复摘要（均已验证）

1. **Bug 3（P0）HTTP 断点重试 416 自愈** — `download.rs` `run_download_impl`：
   fallback(`Ok(v)` 若 `v>received`) → 丢弃 progress → 满速重拉一次。新增 16 测试。
2. **Bug 1（P0）live 录制取消不 finalize** — `engine-hls` 新增 `salvage_merge`：
   统一「收尾 = 清单扫描 + 补洞 + 顺序合并」，worker 的 Cancelled 分支调 `port.finalize`
   （trait 默认 no-op，HLS port 覆写）。新增 18 测试。
3. **Bug 2（P1）BT per-task 限速静默 no-op** — 双管：
   - scheduler `set_task_limit` 对 `bt://`、`magnet:` 在**持久化之前**返回
     `TaskError::Unsupported`（CLI/REST/MCP 三面继承响亮报错）；
   - 顺手修复 QA-E2E Bug 2 同源问题：HLS/FTP port 接入 `TaskBudgets`
     （row-seeded local bucket + live poke + session 结束 drop），
     per-task 限速从「仅 HTTP 生效」扩为「HTTP/HLS/FTP 生效」。

固化资产：`scripts/e2e/`（common.sh + ftp/hls/http-kill9/bt-debian + README）——
真网络场景可重复，HLS Part 2 已从 revert-red 守卫转正常回归守卫。

## 3. 反思（R3 本体）

1. **「功能默认值」陷阱**：Bug 2 的根因是 port trait 的
   `set_task_limit` 默认空实现——对不支持限速的引擎是诚实语义，但被
   HTTP 之外的所有 port 无差别继承后，语义漂移成「假成功」。
   教训：trait 默认实现只该服务「无意义」场景；有副作用语义（限速）的
   方法，不支持方必须显式声明（`Unsupported`），而不是静默吞掉。
   本次把「显式拒绝」放进 scheduler 层（单一 choke point），port 层
   只管技术实现，方向正确。
2. **同构即复用**：HLS/FTP 限速修复直接镜像 HttpAutoPort 的
   budget registry 模式，抽出 `TaskBudgets` 后三 port 共用一套语义
   （row-seed / live-poke / session-drop）。写第三个 copy 之前抽公共件，
   比事后去重便宜一个数量级。
3. **e2e 的一次性脚本幻觉**：R1 的 6 个场景当时是手打命令行；
   「下次回归怎么办」这个问题促成了 scripts/e2e/。判定标准：
   发现 bug 的手段若不可重复，bug 修了也没守卫。
4. **416 自愈的正向价值**：上游 ETag 变更、服务器不支持 Range 且
   忽略 If-Range 时，客户端在协议层面自愈比把失败上抛更符合下载器
   本分。边界（只重试一次、fallback 后 progress 清零）已在测试钉死。

## 4. 遗留与去向

- **BT per-task 限速**：BACKLOG（librqbit 无 socket 级控制点；
  可能方向：chunk 到达后延迟投放模拟整形，代价是吞吐）。
- **公网 e2e 波动**：hls/http/bt 脚本依赖外部源，失败先换镜像重跑；
  ftp.sh 全本地，可作 CI 候选（pyftpdlib 依赖待议）。
- 下一步：#36（MCP 工具验收 e2e）或用户指定的其他里程碑。
