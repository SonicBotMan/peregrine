# M4-b2 R2' 复审处置（修复轮验证）

R2'（delegate del_mu182htn_1pta）：**需修复**——2 P0 + 1 P1 + 5 P2。全部处置如下，251/251 绿。

## P0

- **P0-1 magnet hash 回退绕过引用计数**（detach 返回 None 时 resolve(url) 走 magnet_locator 直接 delete(hash, true)，删掉兄弟任务还在用的数据）→ `detach` 改三态 `enum Detach { Unknown, Held, Last(usize) }`；purge 按态路由：Last→delete(id, purge_files)；Held→无操作；Unknown→hash 回退**前**先 `holds_id` 校验（hash→session live id→registry 任一 url 仍映射该 id 则拒删，走 remove_sink）。回归测试 `magnet_hash_fallback_respects_live_sibling`。
- **P0-2 引用计数按去重 url 而非按任务**（同 url 双任务——同种子同目录不同 sink 文件名——合法共存但 `HashSet` 只记 1 引用；先 remove 即删共享数据）→ `urls: HashMap<String, usize>` refcount。回归测试 `same_url_two_sinks_refcounts_purge`。**修复中发现的第三缺陷**：Held 时 `url_to_id.remove(url)` 已执行，后续同 url purge 全落 Unknown——改为仅 Last 才移除映射（新回归测试当场抓住）。

## P1

- **scheduler.remove() 无条件 port.purge() + BtAutoPort 硬编码 purge_files=true**（remove 即删种子数据，与自身文档矛盾）→ 最小改动：BtAutoPort.purge 传 `purge_files=false`（engine purge 语义收窄为"清引擎侧状态"），完整 REST/CLI/MCP purge 贯通按计划归 M5.1（其 P0-2 方案已固化）。

## P2

- P2-1 offline() 仅关 DHT 仍发 LSD 多播/tracker announce → `disable_local_service_discovery`/`disable_trackers` 同步关闭。
- P2-2 「处置文档缺失」→ **勘误：误报**。`M4-b2-R2-disposition.md` 在盘（4013B，md5 30d7aef0，落盘后即时校验过）；R2' 运行环境未能读到（delegate 文件系统视图问题）。文档本身无缺。
- P2-3 add_source 大小写敏感 starts_with 与 is_bt_source 分歧（MAGNET: 误入本地路径分支）→ 统一 Url::parse 按 scheme 分流。
- P2-4 CLI 主依赖 scheduler 仅为 init_tls（链入 librqbit 全家）→ CLI 是纯 UDS thin client 从不发 TLS——直接删调用，scheduler 挪 dev-deps。
- P2-5 跨重启非 magnet purge 数据残留 → B59（BACKLOG）。

## 验证

- `cargo test --workspace`：**251/251 绿**（engine-bt 9：+2 P0 回归）
- clippy 0 warning；fmt 干净
- 修复过程发现并当场修掉 **detach-Held 删映射** 的级联缺陷（第三次"共享 purge 语义"缺陷，同一回归测试矩阵覆盖）

## 遗留

- M5.1（purge 契约贯通 REST/CLI/MCP + socket 同源 + tcp 默认）为下一轮，方案已固化于 M5.1-R2-disposition.md。
- M6-a 打包（tarball + deb，version 2.0.0-alpha.1，strip=true）已随本轮落地。
