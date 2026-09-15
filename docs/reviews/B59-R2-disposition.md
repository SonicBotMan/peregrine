# B59 R2 处置（del_mu2mx708_p138）

R2 为超时强杀前的完整发现集（SIGTERM，结论部分缺失，发现全部可采纳）。

| 发现 | 级别 | 处置 |
| --- | --- | --- |
| 侧表记 output_folder，跨重启 purge `remove_dir_all` 整个下载目录，连坐同目录其他任务/用户文件 | P0-1 | **已修**：register 改记精确数据实体 `output_folder.join(torrent_name)`（`download` 内经 `session.get(id).name()` 解析；magnet 未解析 → None → 不记，跨重启退化为 sink 兜底）。测试加 bystander 回归断言（共享目录中无关文件必须幸存）。 |
| Last 分支（同 session purge）在 detach 内无条件清侧表，但 purge_files=false 语义要求保留数据定位 | P1-1 | **已修**：detach 不再碰侧表；消费只发生在「purge_files && 数据删除成功」两处（Last：`session.delete` ok 且 purge_files；Unknown：`remove_sink(data)` 成功后）。新增回归 `b59_keep_files_purge_neither_deletes_data_nor_consumes_entry`。 |
| Unknown 分支先清条目再删文件，EACCES 瞬态失败 → 条目已丢 → 永久泄漏 | P1-2 | **已修**：`consume_side_entry` 仅在删除成功后调用；失败路径 `?` 上抛，条目保留供下次重试。 |
| 侧表命中删除后仍落入 by-hash / sink 兜底继续删 | P1-3 | **已修**：命中分支删完即 `return Ok(())`——同一份数据只有一个正确删除目标，兜底只服务侧表从未覆盖的行。 |
| flush 非原子（直接 write），崩溃窗口可清零侧表 | P2-1 | **已修**：tmp+rename 原子写，失败清理 tmp 并 warn。 |
| 多 daemon 实例共享侧表文件的覆盖窗口 | P2-2 | **已知限制，不改**：与 tasks.db 同目录同单写者假设一致（一数据目录一 daemon）；文档注释声明。 |
| register 高频路径全量重写 IO 放大 | P2-3 | **可接受**：tens-of-entries 量级，json 全量重写毫秒级；注释声明。 |

修复后：engine-bt 12/12 绿（b59 三条：跨重启精确删+连坐幸存、同 session 消费、keep-files 保留）。

教训（并入 R3）：R1 自查的「存疑点①」正是 R2 的 P1-1——自查已嗅到 purge_files=false 语义缺口但未追到底；「删数据的定位器」与「删数据的行为」生命周期必须绑定审查。
