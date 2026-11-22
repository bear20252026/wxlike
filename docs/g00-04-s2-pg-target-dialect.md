# G00-04 S2 — PG 目标表集合的口径修正（2026-11-21）

## 结论

之前 `schema/registry/pg-table-map.json` 把 `schema/proposals/**` 下所有
`CREATE TABLE` 都当成 PostgreSQL 目标，导致 **48 张缺失 / 67 张 direct**
这个口径偏低且分类错误。按方言重新核对：

| 提案组 | 方言 | 依据 |
| --- | --- | --- |
| G02 `identity.sql` / `relations.sql` / `governance.sql` / `security.sql` | SQLite | `INTEGER PRIMARY KEY AUTOINCREMENT`；执行点是 `db.rs` 迁移链 |
| G04 `0001_media.sql` | SQLite | 执行点是 `MediaDb::open`，与平台共用一个 SQLite 文件 |
| G05 `content.sql` | SQLite | 执行点是 `Db::migrate_g05()` |
| G06 `0001_ledger_commerce.sql` | PostgreSQL | `BIGINT` / `TEXT` / 不可变触发器 / `BEGIN; COMMIT` |
| G07 `*.sql` | PostgreSQL | 同上 |
| G09 `0001_calls.sql` | PostgreSQL | PG 兼容类型 + `CREATE INDEX IF NOT EXISTS` |

## 修正后的数字

- 真正的 PG DDL：G06 + G07 + G09，共 **61 张表**。
- 平台主库 116 张业务表里，和这 61 张同名的只有 **20 张**（全是 `commerce_*`）。
- 因此 PG 侧真实缺口是 **96 张**：89 张 port + 7 张 rewrite。
- 原来的 48 张不是“全部缺口”，只是“在错误的 PG 全集下看起来缺的表”。

## 对已拍板决策的影响

四个核心决策不受影响，反而更必要：

- `users` / `sessions`：真实 PG 侧没有，M011 必须建。
- `media`：G04 只是 SQLite DDL；M012 必须先把 `media_object` / `media_ref`
  落成 PostgreSQL 表，兼容视图才有底。
- `ledger` / `wallets`：G06 的 ledger 表是真的 PG；SQLite 的 `wallets` /
  `ledger` 仍要迁移，M013 的 staging + 批次脚本就是为此写的。
- 原来被误判为 direct 的 `friends` / `groups` / `favorites` / `moments` 等，
  实际上都要进 port/rewrite 清单；不要按旧 map 的 48 张排期。

## 下一步

`pg-table-map.json` 需要按 `pg_target_dialect` 重算；在重算完成前，旧文件的
`counts` 只能当历史草案，不能当割接范围。M011 已按真实 PG 目标写，不依赖旧
map 的 48 张统计。
