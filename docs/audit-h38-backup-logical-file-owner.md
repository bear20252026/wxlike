# H-38：后上传者的逻辑文件不进 backup

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `5d69766`。

审计原文：`AUDIT-60895e18.md:416-421`。备份按 `media_object.owner` 选条目；
B 上传 A 已存在的同内容时 `media_object` 仍归 A，但 `media_file` 归 B。B 导出
备份时看不到自己的文件。

## 修复

`crates/features/media/src/backup.rs::owner_entries`：

- 查询改为从 `media_file f` 出发，`JOIN media_object o ON o.object_id = f.object_id`
  且 `o.status = 'ready'`，条件 `f.owner = ?1`。
- `GROUP BY o.object_id, o.size, o.mime, o.created_at`：同一用户多次上传同一
  内容时物理 blob 只打包一次；名字/分类取 `MIN(f.name)` / `MIN(f.category)`，
  保证 manifest 每条对象只有一条 entry。
- 行数上限、索引编号、`total_bytes` 与 `payload_bytes` 全沿用现有路径；因为
  object 已去重，payload 不会重复写同一 blob。

## 证据

`cargo test -p wxlike-media --test g04`：27 passed。

新增 `backup_includes_a_second_owners_logical_file_for_the_same_blob`：

1. alice 上传字节 X，bob 上传完全相同的 X（秒传路径，`UploadStatus::Completed`，
   `media_object` 仍是 alice，bob 有 `media_file`）；
2. alice 的 plan 有 1 条，bob 的 plan 也有 1 条，`object_id` 相同、`name=b.txt`；
3. `bob_plan.payload_bytes == X.len()`，物理 blob 只算一次；
4. bob 导出、在另一实例 `import_backup("bob", ...)`，`imported=1`，
   `authorize_read("bob", object_id)` 成功。

`g04` 原有 backup roundtrip、manifest 边界、restore/rollback 用例继续通过。

## 边界

- 备份现在以 `media_file` 为逻辑单位；没有 `media_file` 行的 object 即使
  `media_object.owner` 是该用户，也不会进备份。这是有意的：备份恢复的是
  "文件"，不是物理缓存对象。
- 同一用户同一对象有多条 `media_file` 时只保留一个名字（字典序最小）。如果
  产品要保留每个逻辑文件名的独立恢复记录，需要把 manifest 的 entry 模型从
  "对象"升级为"文件"，并让 payload 偏移可复用；当前模型下不这样做。
- 跨用户同内容时两个用户各自备份同一 blob，物理存储只在各自包内出现一次；
  全局去重不是备份格式的职责。
