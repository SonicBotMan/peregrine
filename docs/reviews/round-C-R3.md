# 轮C R3 反思（B52/B56/B50）

## 交付

- B52：FTP resume 目标缺失 → B19 同款友好文案（engine-ftp open_sink append 分支）。
- B56：FTP SIZE/RETR 偏差按 X<Y / X>Y 分流措辞（增长/收缩各报其因）。
- B50：`fetch::is_permanent`（4xx 且非 408/429）短路重试；`retry_blips` 泛型核心；EXT-X-MAP init 段升级为 3 次重试；回归测试 `permanent_404_segment_is_not_retried`（计数恰好 1 次）。budget「二次计费」经源码定性为限速语义非配额，**不修**（R2 确证）。
- R2（del_mu2nzpq6_v98h）：同意提交，0 P0/P1；P2-1（attempts=0 防御）已顺手修（`.max(1)`），P2-2/3 记 B62/B63。

## 教训

1. **R2 复核「不修」定性是流程的价值所在**：B50-b 若照单全收就是为不存在的 bug 写补丁——`RateBudget::acquire` 是限速闸门，重传再计费诚实。反向教训：R2 观察也可能基于对语义的误读，主 agent 必须回源码定性再动工。
2. **文案也是契约**：`short body` 前缀被 `ftp.rs:409` 断言依赖——改错误串前先 grep 调用方与测试，保留可兼容前缀、只扩展后缀。
3. 小项轮的价值在「批量降维」：三个 P2 项一小时内清完（文案×2 + 重试姿态），不值得单独成轮；但重试核心抽取（retry_blips）让 init/segment 共享同一取消/退避骨架，是结构收益。

## 状态

轮C 完成（workspace 288/288，clippy 0，fmt 过）。剩余：轮D（B39/B41/B42 UI/发布）、轮E（B36 行键 + LM 解析）。
