# PG 割接登记（G00-04）

本文件是 G00-04（SQLite → PostgreSQL，issue #104）的登记与口径说明。它不引入
新的编号规则：**PG 迁移沿用 `schema/registry/migrations.json` 的同一编号线**
（`M001` 起递增，一组同批 DDL = 一个编号，编号永不复用、永不重排）。

## 编号线沿用

- 编号事实源：`schema/registry/migrations.json` 的 `migrations` 数组与 `next_id`。
- 政策（`migrations.json` 的 `policy`）：`append_only`（已发布编号内容不可修改，
  要改就新编号 + 前向修正迁移）、`idempotency`（每个迁移必须可重入，中断重跑安全）、
  `rollback`（破坏性变更必须附回滚说明）、`registration`（编号由 G00 串行分配）。
- SQLite 侧 M001–M009 已发布且内容冻结；PG 侧从 `M010` 起继续追加，不复用、不重排。
- 编号与 SQLite 条目共用一条线，便于「同一编号在两种存储上各有一份实现/入口」对账。

## M010 = ledger / commerce PG DDL

- 编号：`M010`（`next_id` 已推进到 `M011`）。
- DDL 唯一来源：`schema/proposals/G06/0001_ledger_commerce.sql`。该提案即 **CI 可执行入口**：
  它已自带 `BEGIN;` / `COMMIT;`，并全部使用 `CREATE TABLE/INDEX IF NOT EXISTS`、
  `CREATE OR REPLACE FUNCTION`、`DROP TRIGGER IF EXISTS` + `CREATE TRIGGER`、
  `INSERT ... ON CONFLICT DO NOTHING`，因此可被 `psql` 直接连跑两次而不改结构。
  正因为提案已具备事务与可重入包装，本次**没有**再新建 `schema/registry/pg/*.sql`
  包装层（改动面最小）；若将来提案失去可重入性，再补一个只做 `BEGIN; \i 提案; COMMIT;`
  的执行入口，DDL 仍只有一份。
- 覆盖对象：`ledger_accounts` / `ledger_journals` / `ledger_entries` / `ledger_holds` /
  `ledger_closings` / `ledger_closing_balances`，以及 `commerce_*` 共 26 张表。
- 不可变兜底：`ledger_entries` / `ledger_journals` / `ledger_closings` /
  `ledger_closing_balances` 四张表有 `UPDATE`/`DELETE` 触发器；`ledger_entries` 另有
  「入账分录集冻结」触发器（`entry_count` 在入账时声明，事后追加分录被拒）。
  `ledger_closing_balances` 复用 `ledger_closings_immutable()`，报错文案与日结快照同源。
- CI 验证：`ci/jobs/build/job.yml` 的 `pg-migration` 作业（gate matrix C12）在
  `postgres:16` service container 上连跑两次 DDL，再用
  `ci/jobs/build/pg_migration_asserts.sql` 断言表/触发器/函数存在与不可变性。
  这只证明 DDL 在稳定版 PostgreSQL 上成立，**不代表**生产 PG profile 已落地。
- 回滚：全部为新增对象，无 down migration；说明见 `schema/proposals/G06/ROLLBACK.md`。

## S3 端口化进度（2026-11-25）

- 平台 `Db` 已有 Postgres profile（sqlx 池 + 事务端口）；`StorageProfile::dialect()`
  是唯一方言判据，`StorageConnection` 亦暴露只读 `dialect()` 供少数表名差异分派。
- 平台域写入已全部改走 `StorageConnection`：notes / stickers / recovery / status /
  friends / live / articles / moments / video_feed / wallet / account / 会话轮换 /
  关系闸门（`may_message`、`shared_group_ids`）。旧 `content_index::*_tx`、
  `favorites::revoke_source_tx`、`inbox::notify_tx` 与
  `StorageConnection::sqlite_tx` 已删除。
- 会话表存在物理名差异：SQLite 用平台 `sessions`，PG 用 M014 的
  `identity_session_token`；`repo::sessions` 的设备名/设备 id/sid 更新/旧会话清理
  均按 `dialect()` 选表（2c40147f）。
- 唯一仍持 `Db::lock()` 的生产面是 `domain/identity_admin` 的 5 个治理入口。原因
  不是平台侧不想迁，而是 identity 治理函数是 `&Connection` 签名，且 M014 没有
  `identity_user_governance` / `identity_governance_audit` /
  `identity_governance_label` 三张表；审计 `seq` 依赖 `last_insert_rowid()`，
  `require_user` 查 `sqlite_master`。这三项要一起做 M015 + 端口化 + PG 断言。
- 因此 `G00-04-PG` 保持 open：剩余工作是治理面端口化、SQLite→PG 全量导入、
  PG restore drill 与生产版本/拓扑裁决；本文件不再把「51 个旧 repo 函数」当作
  未完成项。

## S2 设计决策（开工前需拍板）
- **PG 驱动**：`sqlx`（编译期校验、自带池）vs `tokio-postgres`（更贴近现有手写 SQL 风格）。现有 `crates/platform/src/domain/repo/*.rs` 是手写 SQL，`tokio-postgres` 迁移成本可能更低；`sqlx` 的编译期查询校验在 132K 行手写 SQL 上收益有限。
- **双实现策略**：trait 双实现（`PlatformXxxRepo for SqliteRepo / PgRepo`）vs 条件编译 vs 单一 `Storage` 端口。trait 双实现类型与编译时间翻倍；条件编译会让"同一份逻辑两种方言"难以对账；单一端口需在 repo 层引入方言抽象。**建议先做单一 `Storage` 端口 + SQLite 实现跑通，再落 PG 实现**——方向性决策，需 owner 拍板。
- **停写窗口**：切换必须"停写 > 导入 > 影子读对账 > 切读 > 启用写"；窗口时长要有测量（`data-import` 对账脚本产出），不允许无测量的在线双写。
- **真实版本**：CI 冒烟用 `postgres:16`，生产版本按部署环境裁决（§3 S6）。

## 后续待办（未落地，属 G00-04 剩余工作）

1. **PG repository**：platform 的 `StorageProfile` / `Db` PG profile 与平台域
   repo 端口化已落地；**identity 治理**仍为 SQLite-only（见 S3），commerce/ledger
   的 PG repository 仍待做。
2. **数据导入**：`composition/data-import/` 不存在；需做计数/摘要/余额守恒对账与
   中断可重跑，先在空库/合成库验证。
3. **restore drill**：现有 drill 仅 SQLite（`deploy/production/restore-drill.sh`）；
   PG drill 未做。`deploy/runbooks/backup-restore.md:70-71`、
   `deploy/runbooks/migration.md:37-38` 明确 defer：PG 割接后才改 `pg_dump/pg_restore`
   并重新演练，在此之前不得声称支持 PG 备份/回滚。
4. **真实版本裁决**：CI 用 `postgres:16` 只是稳定版冒烟；生产 PG 大版本、连接池
   （sqlx / tokio-postgres）与部署拓扑由 G00-04 后续决定。

## 与 SQLite 的一致性对账口径

- 对账目标是「同一编号、同一业务语义在 SQLite 与 PG 上等价」，而不是逐字节相同。
- 计数与摘要：按域表对齐行数、关键唯一键（`business_key` / `operation_id` /
  `request_digest`）与金额汇总；余额只能由 `ledger_entries` 聚合导出，不存余额列。
- 不变量：分录不可变（四表 UPDATE/DELETE 被数据库拒绝）、journal 配平
  （`SUM(amount_minor) = 0`，按 currency 分组）、日结快照与逐账户余额一致。
- 编号口径引用 `migrations.json` 的 `policy`；工作顺序与验收依赖引用
  `integration-requests/G00/G00-04.yaml`（`plan_after_unblock` 五步、`acceptance_carryover`）。
- 发布门槛：`ci/jobs/policy/known-gaps.json` 的 `G00-04-PG`（`blocks_release=false`）
  与 `integration-requests/G16/G16-05.yaml` 的 `G16-05-PG` 需对齐；`G00-04-PG` 的
  `close_when` 是「PG profile 落地 + restore drill 跑 PostgreSQL」，本文件完成
  M010 与 CI 冒烟**不等于**该 gap 关闭。
