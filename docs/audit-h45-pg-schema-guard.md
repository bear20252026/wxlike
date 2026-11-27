# H-45：PostgreSQL 启动只 `SELECT 1`，不校验 schema

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `8e06804`。

审计原文：`AUDIT-60895e18.md:474-479`。审计时 PG 打开路径只探活，空库/半迁移库
可以启动，首个业务查询才失败。

## 修复

`crates/platform/src/storage.rs`：

- `PgStorage::open` 现在在 `PostgresPool::open` 后依次执行：
  1. `require_objects(required_pg_objects())`：`public` 里必须存在
     `crates/platform/tests/schema_snapshot.txt` 的全部表/视图（沿用已有实现）；
  2. **新增** `require_columns(required_pg_columns())`：快照中的每个
     `table.column` 必须能在 `information_schema.columns` 找到。
- 列检查用 `unnest($1::text[])` + `NOT EXISTS`，不依赖 PG 私有 catalog；
  缺列时返回受控 `PlatformError::Storage`，错误里带缺失数、总数和前 20 个
  列名（其余折叠为 `(+N more)`），不 panic。
- `required_pg_columns` 从同一份快照解析并去重排序，避免表/列两份真源漂移。

## 证据

`cargo test -p wxlike-platform --lib`：74 passed，20 ignored。

新增 `required_pg_columns_cover_every_snapshot_column`：

- 快照列数 ≥700（实际 704）；
- 关键列 `users.id`、`conversation_event.conv_id`、`wallets.balance` 存在；
- 列名去重且有序。

`required_pg_objects_cover_every_snapshot_table` 继续通过（≥100 表）。

## 边界

- **没有真实 PostgreSQL 执行**：本机仍无 `psql`/`docker`，`WXLIKE_TEST_PG_URL`
  未设置，新增 SQL 的实际执行只在 CI 的 `pg-migration` 或配了 URL 的
  `pg_storage_exposes_postgres_connection` 里发生。列检查的纯列表逻辑已测，
  但 SQL 语法/权限需要在 PG 19.0 run 上确认。
- 项目没有独立的 catalog schema-version 行；这里的"版本"以 704 列快照完整性
  表达：少表或少列即拒绝启动。类型、NOT NULL、主键、触发器、索引形状仍由
  `pg_schema_parity` / `pg-migration` 断言负责，不在启动守卫里重复。
- 列存在但类型错误时仍可能通过启动；更严格的类型比对需要 PG 端 catalog
  映射表，当前不做。
