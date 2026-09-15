# 轮E R1 自查（B36 遗留：重定向行键统一 + LM 精确解析）

对象：crates/engine-http/src/auto.rs、crates/api/src/download.rs、tests/auto.rs、api 内测、docs/BACKLOG.md。
（工作区另含轮D 的 UI/CI 变更，独立提交，不在本轮范围。）

## 修复一：降级路径行键漂移（B36-R2 P2-3）

- 病灶：fresh probe 分支把 `job.url` 改写为 probe final URL；若 segmented 会话触发 `SingleStreamRequired`，`run_with_downgrade` 的重启 job 以 final URL 写 validator-only 行——而所有读侧（scheduler resume_job、download_auto Route 1/2）按 original URL 查 → 行永远不可达，退化为无 validator 续传。
- 修法：`download_auto` 在改写前存 `original`，传入 `run_with_downgrade(…, original_url, …)`；重启 `fresh.url = original_url`。删 segment 行仍用 `job.url`（segment 行本就以 final 写入）——顺序上先删后改，键各自正确。
- Route 1 分支调用点传 `job.url.clone()`（Route 1 的 job.url 本就是 original，行为不变）。
- 测试 `downgrade_validator_row_keys_original_url`：HEAD /file 302→/real；confirm 206、worker 全段 200（背叛）→ 降级；断言行键=original（total=NULL, etag="v1"）且 final 键无孤儿行。revert-red 语义：修复前该行断言必挂。

## 修复二：Last-Modified 精确解析（B36-R1 遗留）

- 病灶：`from_wire` 对非引号串一律重建为 `LastModified`——垃圾串当 If-Range 发出，必然失配 → 200 全量重放（安全但假装有 validator 且浪费）；`from_probe` 对 LM 零校验。
- 修法：新增 `valid_http_date`（api 层 pub）：RFC 7231 IMF-fixdate 29 字符定长校验（weekday 表、日 01-31、月表、4 位年、时≤23/分≤59/秒≤60 含闰秒、GMT 后缀；obs-date 旧形状拒绝）。`from_wire` else 分支与 `from_probe` LM 分支共用；非法 → None → 无 validator 退化（诚实的安全重放）。
- 无新依赖（不用 chrono/httpdate——定长格式无变宽字段）。
- 行为变更声明：`from_wire("v1")` 旧版重建为 `LastModified("v1")`，新版返回 None。既有 DB 行中如有垃圾 LM wire，读回变 None——安全方向（且旧值发出的 If-Range 本就必失配）。
- 测试：4 组单测（canonical 接受×4、拒绝组：时/分/秒/日越界、假月、RFC 850/asctime 旧形状、UTC 后缀、小写、空格补日、空串；from_probe 过滤非法 LM + 合法 LM 透传）。

## 文档：P2-4 WONTFIX 定性

镜像 etag 抖动（FileETag INode 类）：original 键行的 etag 每次会话后刷新，抖动代价 = 每次轮换一次安全全量重放后自适应——任何键选择都躲不掉（final URL 同样轮换）。BACKLOG B36 条目已收口为 RESOLVED + WONTFIX 附注。

## 验证

- `cargo test --workspace`：43 套 292/292 绿（api 10/10、auto 8/8）。
- `cargo clippy --workspace --all-targets`：0 warning；`cargo fmt`：过。

## 存疑

- `valid_http_date` 不校验 weekday 与日期的一致性（需闰年历法数学，无安全收益）——lenient 已注明。
- 降级重启后 final URL 与 original 的 segment 行删除（`store.get_task(&job.url…)`）——若 probe 追到的 final 与 worker 使用的 final 不同（重定向链中途变化），删行可能 miss。降级路径删行本就是 best-effort（注释已声明），未变。
- run_with_downgrade 签名加参（私有函数，两个调用点都改）——无外部 API 影响。

R1 结论：可交 R2（重点：fresh.url 覆盖后 segment 行删除顺序的正确性；valid_http_date 的边界；from_wire 行为变更的既有数据兼容性）。
