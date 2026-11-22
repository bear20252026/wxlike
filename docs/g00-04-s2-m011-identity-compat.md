# G00-04 S2 — M011 身份兼容层与第一批 repo trait

状态：M011 已登记；第一批 repo trait 已落盘；M012（media 视图）已登记为草案，PG 写分支已预留但尚未端到端实跑；M013（ledger/wallets 回填）已登记。

## 1. 决策落地

| 决策 | 落点 | 说明 |
| --- | --- | --- |
| `users` 保留同名表面 | `schema/proposals/G02/0002_identity_pg_compat.sql` | 物理表是 `identity_user`；`users` 是视图 + INSTEAD OF 触发器。旧 `INSERT/SELECT/UPDATE/DELETE` 不改签名 |
| `sessions` 保留同名表面 | 同上 | 物理表是 `identity_session_token`；`sessions` 是视图 + 触发器；刷新链落 `identity_refresh` |
| `media` 先走兼容视图 | `schema/proposals/G04/0002_media_pg_compat.sql` | 读兼容已就位；旧 upsert 不能作用在视图上，见 §4 |
| `ledger/wallets` 严格分批迁移 | `schema/proposals/G06/0002_ledger_wallets_backfill.sql` | staging + `migrate_wallets_batch()`，逐行一笔 journal，严格模式余额不等即中止 |
| 余额实时聚合 | 同上 | `wallet_balances` 视图；不建物化余额列 |

## 2. M011 关键约束

**PostgreSQL 的外键不能指向视图。** `users` 变成视图后，新表的 `REFERENCES` 必须写：

```sql
REFERENCES identity_user(user_id)
```

不能再写 `REFERENCES users(id)`。`sessions` 同理，新表引用 `identity_session_token(token)`。

这不是风格问题：只要有一张新表按旧写法引用 `users`，`CREATE TABLE` 当场失败。

`users` 视图的写触发器会把整行回写 `identity_user`。旧代码只 `UPDATE users SET name=?` 时，视图 `NEW` 行仍带出当前 `pw_hash/created_at` 等列，所以不会把别的列写成 NULL。视图列还补了旧表的 DEFAULT（`pw_algo='pbkdf2'`、`totp_enabled=0`、空串资料列），否则 `INSERT ... RETURNING` 会拿到 NULL，与 SQLite 返回口径不一致；触发器里的 `COALESCE` 是第二道防线。

`sessions` 视图的 `rowid` 用 `ctid::text` 兜底，只服务旧的一条有界过期清理 SQL；它不能当稳定游标。

## 3. 第一批 repo trait

文件：

- `crates/persistence/src/users.rs`（trait + DTO，跨 crate 单一端口）
- `crates/persistence/src/sessions.rs`（trait + DTO）
- `crates/platform/src/domain/repo/users.rs`（`SqliteUsersRepo` + `PostgresUsersRepo`）
- `crates/platform/src/domain/repo/sessions.rs`（`SqliteSessionsRepo` + `PostgresSessionsRepo`）
- `crates/platform/src/storage.rs`（`PostgresPool`：sqlx 连接池 + 端口同步桥）

做法：

- 一个域一个 trait：`UsersRepo` / `SessionsRepo`；trait 放在 `wxlike-persistence`，platform 已依赖，identity 在真正实现 PG 端口时再加同一路径依赖，避免出现两套定义。
- trait 用关联类型 `Error`，不把 `PlatformError` 硬编码进去；SQLite/PG 实现都把 `Error` 设为 `PlatformError`。
- SQLite 实现复用旧 SQL；PG 实现直接写 `identity_user` / `identity_session_token` 物理表，不依赖兼容视图。
- **没有改任何现有 repo 函数签名**；旧调用点一行没动。
- trait 是公开端口；四个实现仍是 `pub(in crate::domain)`，handler 层编译期看不到实现，绕不过去。
- `#[allow(dead_code)]` 是刻意的：seam 先就位，调用点分批迁；不为了消警告一次性改 56 处 users SQL。
- `PostgresPool` 用 sqlx + 专属 multi-thread runtime；端口方法是同步 trait，调用前必须处于阻塞上下文（`Db::blocking` / 普通线程），不要在 async 任务里直接调。

PostgreSQL 实现已在本机 PG16 实跑：`PostgresUsersRepo` / `PostgresSessionsRepo` 的 `#[ignore]` 测试均通过；CI 的 `pg-migration` 作业也加了同一组测试。

验证：

```powershell
# SQLite 端口
cargo test -p wxlike-platform --lib domain::repo::
# PostgreSQL 端口（先准备 M010-M013 的 PG16）
$env:WXLIKE_TEST_PG_URL='postgresql://postgres:postgres@127.0.0.1:55432/wxlike'
cargo test -p wxlike-platform --lib domain::repo:: -- --ignored
```

## 4. media 的唯一硬阻塞

`domain::media` 的入库语句是：

```sql
INSERT INTO media ... ON CONFLICT(id) DO UPDATE ...
```

PostgreSQL **不支持** `ON CONFLICT` 作用在视图上。因此：

- `media` 兼容视图的读、普通 INSERT/UPDATE/DELETE 已写；
- 原 `ON CONFLICT` 语句不能在 PG 视图上执行，这是原来的硬阻塞；
- 两个候选方案：
  1. 把这一条 SQL 改成普通 `INSERT`，让 `media_insert_trg` 在 `media_object` 上做 `ON CONFLICT`（已采用，且不改 repo 签名）；
  2. 不建视图、改同名兼容表；但这会出现双源，和“单一化定义”冲突。

目前选择：**方案 1 已落成条件分支**——`domain::media::store_media` 在 SQLite profile 下保留原 `ON CONFLICT` 语句，在 PostgreSQL profile 下改走普通 `INSERT`，由 `media_insert_trg` 在 `media_object` 上做幂等 upsert。这样没有改任何 repo 签名，SQLite 行为逐字不变；等 PG 连接实现真正跑通后再决定是否完全删掉 SQLite 分支。M012 仍登记为草案，CI 的 `pg-migration` 已覆盖其 DDL 与兼容断言，但 PG profile 目前仍被 `SqliteStorage::open` 拒绝，分支尚未端到端实跑。

## 5. ledger / wallet 迁移口径

`migrate_wallets_batch(p_batch_size, p_allow_adjustment)`：

1. 导入器（`schema/proposals/G06/import_ledger_wallets.py`）把 SQLite `wallets` / `ledger` 行装进 `migration_wallets` / `migration_ledger`；只读 SQLite，CSV + `COPY` 进 staging，不在 Python 里记账。
2. 每批取 `p_batch_size` 个 `pending` 钱包，一个事务。
3. 每条 legacy `ledger` 行 -> 一笔 journal + 两条分录（用户钱包 + `migration:clearing`），金额取反，`SUM=0`。
4. `wallets.balance` 与流水和不一致时：
   - 默认 `RAISE EXCEPTION`，整批回滚；
   - 只有 `p_allow_adjustment=true` 才补 `migration.opening_adjustment`，并标 `adjusted`。
5. `business_key = mig:ledger:<id>` / `mig:wallet:<user>:opening`，重跑跳过已完成 journal。

调用必须在 psql autocommit 下，不要在显式 `BEGIN` 里 `CALL`（过程内部每批 `COMMIT`）。

## 6. 验证清单（本机 PG16 已跑）

- [x] PG 空库执行 M011 两次，第二次无新增对象。
- [x] `INSERT INTO users ...` / `UPDATE users ...` / `DELETE FROM users ...` 均命中 `identity_user`；`deleted_at`、`name` 回写正确。
- [x] `INSERT INTO sessions ...` / `SELECT ... WHERE rowid IN (...)` / `UPDATE` / `DELETE ...` 均命中 `identity_session_token`；`ctid` 伪列可删除。
- [x] 新表 `REFERENCES` 全部写 `identity_user(user_id)`，无 `REFERENCES users(id)`。
- [x] `cargo test -p wxlike-platform --lib domain::repo::`：2 passed。
- [x] `cargo clippy -p wxlike-platform --all-targets -- -D warnings`：绿。
- [x] `cargo fmt --all -- --check`：绿。
- [x] PG 空 schema 按顺序执行 M011 / M012 / M010 / M013 全部成功；`migration_ledger_kind_check` 在新表上生效。
- [x] M012 读/普通 `INSERT`/`UPDATE`/`DELETE` 实测通过；重复 `INSERT` 同名 id 会更新 `media_object.size/mime`，且不覆盖第一次的 `owner`；旧 `ON CONFLICT(id)` 单独在视图上会报 “no unique or exclusion constraint matching the ON CONFLICT specification”（所以 `store_media` 做了 SQLite/PG 分支）。
- [x] `PostgresUsersRepo` / `PostgresSessionsRepo` 在真实 PG16（M010-M013 已应用）上 `--ignored` 测试 2 passed；发现并修掉 `pw_rounds` INT4/INT8 类型不匹配。
- [x] `cargo test -p wxlike-platform --test media`：14 passed（SQLite 上传/下载/去重/Range/缩略图全绿，证明分支没有破坏原路径）。
- [x] `domain::media::store_media` 已按 `Db::dialect()` 分支：SQLite 保留旧 upsert，PG profile 走普通 `INSERT + media_insert_trg`；写入语句已搬到 `Db::blocking`，不在 await 上下文持同步连接锁（ADR-28），`cargo check` / clippy / fmt 通过。
- [ ] `import_ledger_wallets.py` 全链路 `COPY + CALL` 已接入 CI 的 `ci/jobs/build/pg_import_smoke.sh`，待 GitHub Actions 实跑；本机只做过 `--dry-run` / `py_compile`。
- [ ] `ci/jobs/build/pg_restore_drill.sh` 已接入 `pg-migration`（`pg_dump` → 新库 `pg_restore` → 重跑 M010/M011-M013 断言），待 GitHub Actions 实跑；本机无 PostgreSQL 客户端。
- [x] M013 在 PG16 上跑两次：u1/u2 余额 100/50 与聚合视图一致；第二次 0 pending、3 个 journal 不重复；`p_allow_adjustment=true` 的 u3 补差 10 且状态 adjusted。

本机 PG 验证命令（可复现）：

```powershell
docker run -d --name wxlike-m011-test -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wxlike postgres:16-alpine
docker cp schema/proposals/G02/0002_identity_pg_compat.sql wxlike-m011-test:/tmp/m011.sql
docker exec -i wxlike-m011-test psql -v ON_ERROR_STOP=1 -U postgres -d wxlike -f /tmp/m011.sql
# 再跑一次验证幂等
docker exec -i wxlike-m011-test psql -v ON_ERROR_STOP=1 -U postgres -d wxlike -f /tmp/m011.sql
```

