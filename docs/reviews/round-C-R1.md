# 轮C R1 自查（B52/B56/B50：FTP 文案 + HLS 重试姿态）

对象：未提交 diff。engine-ftp/src/lib.rs（+15）、engine-hls/src/fetch.rs（+9）、engine-hls/src/lib.rs（+60）、tests/merge.rs（+58）。

## 逐项

- **B52**（FTP resume 目标缺失文案）：`open_sink` append 分支对 `NotFound` 输出与 engine-http B19 **逐字一致**的友好文案（`resume target missing: … — the partial file was deleted or the path is wrong; start a fresh download instead`）。Truncate 分支不变（新建文件 ENOENT 是真错误）。
- **B56**（short body 措辞）：X>Y（远端在 SIZE 与 RETR 间增长）不再是误导性的 "short body"，改报 `body longer than SIZE: got X of Y bytes (remote grew between SIZE and RETR)`；X<Y 保留 short body 并补原因注解（`remote shrank or connection dropped early`）。
- **B50-a**（永久 4xx 不重试）：`fetch::is_permanent`（4xx 且非 408/429）+ `fetch_part_retry` 命中即返回 + 抽出 `retry_blips` 泛型核心供 init 段（EXT-X-MAP）复用——init 段从单次 fetch 升级为同姿态 3 次重试。
- **B50-b**（budget 二次计费）：**不修——语义核实为正确**。`RateBudget::acquire` 是限速闸门（sleep/park 到配速允许），非消耗配额；重传产生真实流量，再次 acquire 是诚实计费。R2 原始观察混淆了限速与配额语义。
- 测试：`permanent_404_segment_is_not_retried`（axum origin，seg1 恒 404 + AtomicUsize 计数，断言恰好 1 次请求 + 错误含 404）。B52/B56 为纯文案，不写断言字符串的脆弱测试（与 B19 处置一致）。

## 自查存疑

- `retry_blips` 的闭包捕获 dance（每 attempt clone map_uri/cancel2/tmp/init）是为绕 FnMut 限制——可读性略降但局部化。存疑点①：是否有更地道写法（`Fn` + 内部 clone？）——不改，编译器已验证正确性。
- 408/429 保留重试：408 是 origin 侧超时、429 是限流——正是 backoff 的适应症。无争议。
- init 段重试期间 playlist 可能 refresh 换 map？不会——init fetch 在 download_merge 的静态解析段，live 路径的 map 变化由 `live stream switched EXT-X-MAP mid-recording` 既有守卫拦截。

R1 结论：可交 R2（重点：is_permanent 分类边界、retry_blips 的取消语义、B50-b 不修的定性是否成立）。
