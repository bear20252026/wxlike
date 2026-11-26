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

## S3 端口化进度（2026-11-26）

- 平台 `Db` 已有 Postgres profile（sqlx 池 + 事务端口）；`StorageProfile::dialect()`
  是唯一方言判据，`StorageConnection` 亦暴露只读 `dialect()` 供少数表名差异分派。
- 平台域写入已全部改走 `StorageConnection`：notes / stickers / recovery / status /
  friends / live / articles / moments / video_feed / wallet / account / 会话轮换 /
  关系闸门（`may_message`、`shared_group_ids`）。旧 `content_index::*_tx`、
  `favorites::revoke_source_tx`、`inbox::notify_tx` 与
  `StorageConnection::sqlite_tx` 已删除。
- 会话表存在物理名差异：SQLite 用平台 `sessions`，PG 用 M011/M014 的
  `identity_session_token`；`repo::sessions` 的设备名/设备 id/sid 更新/旧会话清理
  均按 `dialect()` 选表（2c40147f）。
- 身份治理的读路径已端口化：admin 鉴权、用户视图、设备读取/撤销、登录闸门、
  会话鉴权与会话签发都走 `SqlExec`（`9ecf5b24..035cee65`）；自增审计 `seq` 由
  `SqlExec::insert_returning` 在 SQLite 用 `last_insert_rowid()`、在 PG 补
  `RETURNING seq`，调用方 SQL 不写方言。
- 治理三表**已在 M011**（`identity_user_governance` / `identity_governance_audit` /
  `identity_governance_label`）；M015 不重复建治理表，只补 identity 域此前遗漏的
  `identity_retry`（跨重发累计试错预算）与过期索引。M016 再补
  `identity_session_token` 的侧车四列（`auth_level` / `features` / `serial` /
  `issued_at`），让刷新轮换能读 `features` 判断临时认证；M011 已有访问行按
  `auth='auth'`、`features=''`、`serial=0`、`issued_at=0` 解释。治理写路径已端口化：
  `suspend` / `unsuspend` / `set_disabled` / `settle_expired` / `revoke_device`
  由 `*_tx` 在同一 `SqlExec` 事务内完成，平台 `Db::with_tx` 提交后才镜像与派发
  事件；群治理 `admin_govern_group` 的 admin 鉴权与 `govern_with` 共用同一事务。
- 身份域其余可复用面也已端口化：同意回执、找回令牌、限速桶与重试预算、设备读取/
  心跳/信任设置、会话列表/心跳/单会话登出/过期清扫、刷新链查询、step-up 只读校验、
  扫码换码都提供 `*_with`（`5863e5c..c57df46`）。SQLite 旧入口保留为薄包装。
- **会话侧车形状仍有差异**：SQLite 的 `identity_session_token` 以 `sid` 为主键、
  与平台 `sessions` 分表；PG 的 M011 表以 `token` 为主键，把访问会话与侧车合并。
  M016 补列后读路径可用，但 `session::open_with` 的侧车写入仍是 SQLite-only，
  PG 上真正签发新会话前需要拆表或改写入形状，未在本轮完成。
- 平台 `domain` 层已经**没有** `self.db().lock()` 生产调用；`Db::lock` 只剩测试
  与过渡辅助在用。服务端偏好也收敛到 identity `preferences::*_with` 一份 SQL
  （`343542f`）。
- `login_challenge` 的下发/校验/重发仍是 SQLite-only，且平台生产路径未直接调用；
  属于可选清理，不阻塞当前切换。
- 因此 `G00-04-PG` 保持 open：剩余工作是真实 SQLite 快照的全量导入、生产 PG
  版本/拓扑裁决与生产环境 restore/drill 演练；治理面端口化与「51 个旧 repo
  函数」不再是未完成项。S4 导入工具链已落地（见下节），但尚未对生产快照执行。

## S4 数据导入工具链（2026-11-26）

- 目录：`composition/data-import/`；入口 `import_all.py`；包内按
  盘点/计划/目标 catalog/CSV 编码/装载/回读摘要/状态续跑/停写窗口分层。
- 默认同名复制；`wallets` / `ledger` 仍跳过，由 M013 导入器做严格回填。
  `users` / `sessions` / `media` 按 M011/M012 可写兼容视图写入。
- 复制前先校验目标：缺表、物化视图、源独有列、目标独有 NOT NULL 无默认列、
  类型族不兼容都会拒绝；不允许“先复制再发现丢列”。
- M023（`schema/proposals/G00/0001_import_control.sql`）新增
  `import_run` / `import_table`，记录源 SHA、data_version、逐表行数/摘要/状态，
  支撑 `--resume` 与失败表重跑；当前 `next_id=M025`。
- CI：`pg-migration` 在 M023 断言后跑 `pg_data_import_smoke.sh`（合成五表：
  父子外键、identity、空表、bytea/CRLF/numeric），验证清空旧行、NULL/空串、
  摘要、续跑和序列复位。`composition/data-import/**` 属于完整矩阵触发路径。
- 停写窗口：`window.json` 记录源 SHA、SQLite data_version、逐表/总耗时；
  前后 SHA 与 data_version 不变才算 `quiescent`，`--max-window-seconds` 可设上限。
- M024（`schema/proposals/G00/0002_import_compat_backfill.sql`）补齐 110 表
  普查发现的四张目标：`channel_role` 角色列改为 TEXT 并补 `granted_by`/
  `created_at`，新增 `friend_tag`、`group_governance_audit`、`relations_audit`。
- 全量形状普查：`composition/data-import/census.py` 可从
  `crates/platform/tests/schema_snapshot.txt` 生成空形状库，逐表校验目标
  catalog；CI `pg_data_import_census.sh` 断言 107 个计划表全部通过，
  `pg_data_import_full_shape.sh` 再对空 110 表快照执行真实清空/装载/回读，
  107 张目标表全部 `verified`（`run_id=ci-shape`）。
- 目标外键排序：catalog 读取 `pg_constraint`，父表先于子表；`identity_user`
  等兼容视图基表映射回 `users` 源表，`users` 身份根固定最先，避免推演顺序
  漏掉 PG-only 外键。
- 边界：本地/CI 只证 PostgreSQL 16 空库形状与合成数据；真实 110 表的行导入、
  生产快照、生产 restore drill 仍未执行。旧 `pg-table-map.json` 已 stale。
- 详细口径与回滚见 `docs/g00-04-s4-data-import.md`。

## S2 设计决策（开工前需拍板）
- **PG 驱动**：`sqlx`（编译期校验、自带池）vs `tokio-postgres`（更贴近现有手写 SQL 风格）。现有 `crates/platform/src/domain/repo/*.rs` 是手写 SQL，`tokio-postgres` 迁移成本可能更低；`sqlx` 的编译期查询校验在 132K 行手写 SQL 上收益有限。
- **双实现策略**：trait 双实现（`PlatformXxxRepo for SqliteRepo / PgRepo`）vs 条件编译 vs 单一 `Storage` 端口。trait 双实现类型与编译时间翻倍；条件编译会让"同一份逻辑两种方言"难以对账；单一端口需在 repo 层引入方言抽象。**建议先做单一 `Storage` 端口 + SQLite 实现跑通，再落 PG 实现**——方向性决策，需 owner 拍板。
- **停写窗口**：切换必须"停写 > 导入 > 影子读对账 > 切读 > 启用写"；窗口时长要有测量（`composition/data-import` 的 `window.json` 产出），不允许无测量的在线双写。当前只有源侧 SHA/data_version/耗时测量，生产窗口长度与外部停写信号仍需 owner 评审。
- **真实版本**：CI 冒烟用 `postgres:16`，生产版本按部署环境裁决（§3 S6）。

## 后续待办（未落地，属 G00-04 剩余工作）

1. **PG repository**：platform 的 `StorageProfile` / `Db` PG profile、平台域
   repo 端口化、identity 可复用面与 relations 治理写路径均已落地；commerce/ledger
   的 PG repository 仍待做。
2. **数据导入**：`composition/data-import/` 工具链已落地（S4，2026-11-27），
   覆盖盘点/计划/CSV/装载/回读摘要/中断续跑/停写窗口；110 表形状普查已在
   PostgreSQL 16 空库全绿。仍需对真实 SQLite 快照执行全量行导入、钱包对账
   与生产停写窗口演练。
3. **restore drill**：CI `pg-migration` 作业已在 PostgreSQL 16 上跑过
   `pg_dump → psql restore → 断言`；生产部署环境的 drill 仍需在割接窗口前实跑。
   `deploy/runbooks/backup-restore.md:70-71`、`deploy/runbooks/migration.md:37-38`
   仍明确 defer：生产 PG 备份/回滚口径随割接一起改并重新演练。
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
