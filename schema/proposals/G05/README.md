# G05 迁移提案（内容 / 社交 / 收藏 / 检索）

**执行权在 G00。** 本目录只有草稿与回滚说明，没有迁移编号——编号由 G00 统一
分配后串行合入，避免并行组各自编号撞车。

## 文件

| 文件 | 覆盖任务 | 新建表 | 改动既有表 |
|---|---|---|---|
| `content.sql` | G05-01 / 02 / 03 / 04 / 05 / 06 | 18 张（drafts / refs / publishers / articles / notes / stickers / video_posts / content_index / notifications / statuses 等） | `moments` 补列 `audience_group`；`favorites` 补列 `source_kind` / `source_id` / `snapshot_label` / `sort_order` / `revoked` |

全部语句幂等（`IF NOT EXISTS`）；两处补列必须先查 `PRAGMA table_info(...)`
再 `ALTER`——SQLite 没有 `ADD COLUMN IF NOT EXISTS`。crate 内的
`Db::migrate_g05()`（`crates/platform/src/db.rs`）已经这么做，G00 可直接复用
同逻辑，也可以把补列写进正式迁移脚本。**注意顺序**：
`idx_favorites_object`（部分唯一索引）必须在 favorites 补列之后建，
否则旧库上建索引会因缺列失败。

## 回滚兼容说明

**原则：新表可以整体丢弃，既有表的补列不删。** 理由：SQLite 删列需要重建表，
重建在生产库上是高风险操作；而多出来的列对旧代码完全无害（旧代码不
`SELECT *`，写入走显式列）。

| 场景 | 回滚动作 | 数据影响 |
|---|---|---|
| 回滚 G05-01 | `DROP TABLE moment_drafts`；`moments.audience_group` 保留 | 未发布的草稿丢失；已发布朋友圈不受影响（分组可见会退化为按 visibility 字符串解释，`groups` 行为不再匹配分组） |
| 回滚 G05-02 | `DROP TABLE article_versions / article_reads / article_reactions / publisher_follows / articles / publishers` | 订阅关系与未读游标丢失；文章正文不可恢复，**需业务确认**后再回滚 |
| 回滚 G05-03 | `DROP TABLE note_versions / notes / user_stickers / stickers / sticker_packs`；favorites 补列保留 | 笔记与版本历史、表情包安装关系丢失；对象收藏行回退为"无 source 列语义"，正文列仍在但 `revoke_source` 不再生效 |
| 回滚 G05-04 | `DROP TABLE video_follows / video_comments / video_likes / video_posts` | 视频号发布记录丢失；`video_refs` 转码产物保留（可重新发布） |
| 回滚 G05-05 | `DROP TABLE notifications / content_index / content_media_refs` | 未读通知丢失（幂等重建：通知不可重放，索引可由发布事务重放）；媒体引用关系需按各内容表重建 |
| 回滚 G05-06 | `DROP TABLE status_reactions / status_background_drafts / statuses` | 活跃状态全部消失（本来 24h 就会过期）；审计里的 `status_expire` 痕迹保留 |

## 与实现的一致性

- 迁移在 `Db::open` 里调用（`migrate_g05`），全部 `CREATE TABLE IF NOT EXISTS`，
  新列只增不删；旧二进制读带新列的库按显式列名查询，不受影响。
- 删除本迁移代码**不会**回收列，也不需要。
- 快照测试：`crates/platform/tests/schema_snapshot.rs`（改动后跑
  `WXLIKE_UPDATE_SCHEMA=1 cargo test -p wxlike-platform --test schema_snapshot`）。
