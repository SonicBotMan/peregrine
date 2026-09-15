# 轮D R1 自查（B39/B41/B42：UI 报错 / sidecar 日志 / deb 依赖校验）

对象：未提交 diff。App.svelte（+34）、app.css（+21）、src-tauri/lib.rs（+45）、desktop.yml（+22）。

## 逐项

- **B39**（UI 操作失败静默）：`act()` 失败 → `banner()` 瞬态错误条（6s 自动清除，点击关闭，`role="alert"`），dev 的 console.warn 保留。`pickGlobal`/`commitCustomGlobal` 两处原本吞进 console 的失败也接 banner。所有任务操作（pause/resume/remove/limit）已汇聚 `act()` 单点——M3-c1 时的架构债这次只改一处就全覆盖。
- **B41**（sidecar stdout 进 void）：`open_sidecar_log` 打开 `<app_log_dir>/peregrined.log`（append；目录不存在则建；任一步失败降级 None + stderr warn，**不阻塞 daemon 启动**）。pump 任务逐行 `writeln!`（unix-ms 时间戳）；`Terminated` 事件也落盘。dev（debug_assertions）保留控制台 echo。日志无 rotation——sidecar 生命周期=app 生命周期，体量是 tracing 级小日志，v1 可接受（B 系不新增）。
- **B42**（deb 依赖完整性）：desktop.yml build 后加「Verify deb dependency closure」step：`dpkg-deb -I` 打印全部 control 字段进 run log；`dpkg-deb -f $deb Depends | grep -qi webkit2gtk` 缺失即 `::error::` + exit 1；无 .deb 产出也 fail（防止 bundle 静默降级）。globstar + 循环覆盖 `bundle/**/*.deb`。

## 自查存疑

- **B41 无法本地编译验证**（dev 容器无 webkit2gtk，src-tauri 非 workspace member——M3-c2 既定约束）。API 依据：`Manager::path().app_log_dir()`（tauri 2 标准路径解析器，返回 Result<PathBuf>）。desktop.yml 的 paths 触发器含 workflow 自身，push 后 CI 即编译验证。存疑点①：若 `app_log_dir` 在当前 tauri 小版本签名不同，CI 会红——属可接受的远程验证回路。
- banner 消息直接用 `e.message`：REST 错误体里 daemon 返回的 message 已是人类可读文案（M2 API 契约），不再二次包装。若底层是 fetch 网络错误（TypeError: Failed to fetch），文案不理想但可辨识——v1 接受。
- B42 的 grep -qi 'webkit2gtk'：tauri 2 deb 的 Depends 历史上是 `libwebkit2gtk-4.1-0`（版本后缀变化）——用 `-i webkit2gtk` 子串匹配避免版本后缀脆弱性。
- desktop.yml 仅 linux job；mac/win 未配置（既有范围，不扩）。

R1 结论：可交 R2（重点：B41 的 tauri API 用法与降级路径；banner 定时器在组件卸载时的清理缺口（无 onDestroy——SPA 单页常驻，可忽略？）；B42 step 的 shell 正确性）。
