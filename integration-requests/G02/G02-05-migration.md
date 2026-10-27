# G02-05（#114）迁移草稿与回滚兼容说明

执行者：**G00**。本文件是草稿与说明，不是执行结果。
`crates/platform/src/db.rs` 与 `schema.rs` 由 G00 与并行组共用（冲突史：G00-02 的
未提交稿曾占用这两个文件并重引入 G02 的表丢失问题），所以本卡**不新增迁移编号**，
只做一次原地补列；请 G00 把下面的语句并入 `schema/proposals/G02/identity.sql`
并在登记表里标注"users.deleted_at 由 G02-05 引入"。

---

## 1. 这次到底动了什么

| 对象 | 变更 | 占迁移编号 | 幂等 |
|---|---|---|---|
| `users` | 新增可空列 `deleted_at INTEGER`（NULL = 未注销） | **否**（原地补列） | 是（`has_column` 守卫） |
| `identity_pref` / `identity_job` / `identity_export_link` | 无变更：三张表已在 `schema/proposals/G02/identity.sql` 里（G02-05 段落） | 已由 G02-01/05 草稿覆盖 | 是（`CREATE TABLE IF NOT EXISTS`） |
| `identity_job.status` | **取值集合**增加 `cancelled` | 否 | — |
| `schema_snapshot.txt` | 多一行 `users.deleted_at INTEGER notnull=0 pk=0` | 否 | — |

### DDL 草稿

```sql
-- 只在缺列时执行（SQLite 没有 ADD COLUMN IF NOT EXISTS）。
-- 先探测：PRAGMA table_info(users); 里没有 deleted_at 才 ALTER。
ALTER TABLE users ADD COLUMN deleted_at INTEGER;   -- epoch ms；NULL = 活账号
```

平台侧的实际落库代码是 `Db::migrate_account()`，由 `Db::open` 调用链尾部执行，
与既有 `migrate_security_columns` 完全同一手法（同一守卫函数、同一时序位置）。

### `cancelled` 不需要 DDL，但必须说明

`identity_job.status` 是 `TEXT NOT NULL DEFAULT 'queued'`，**没有 CHECK 约束**，
所以新增一个取值不需要改表结构。这也是为什么代码里读它必须走
`JobStatus::parse` 并在认不出来时报 `Corrupt`：没有约束的列，值域只有代码在守。
请 G00 在迁移登记里把值域写成文档（`queued|running|done|failed|cancelled`），
**不要**顺手加 CHECK 约束——那会让已经写坏的行在 ALTER 时直接失败，把"可诊断的
500"变成"库打不开"。

---

## 2. 为什么用标记而不是 DELETE

注销的清理顺序（`account.rs::schedule_deletion` → `preferences::request_delete` →
`run_delete_job`）是：

1. 撤销全部会话 + 刷新链 + step-up（`identity_pref` 不动）；
2. 落 `users.deleted_at`；
3. 排一条 delete 作业；
4. worker 在**一个事务**里逐域 DELETE、对保留域做脱敏 UPDATE、最后匿名化用户行。

`deleted_at` 只回答一件事："这个账号不再属于活人"。所有发凭证的入口
（口令校验 `verify_credentials`、会话签发 `issue_session`、令牌解析
`lookup_token`）都按它 fail-closed，所以**第 2 步做完，数据就已经取不到了**，
不必等 worker 跑完第 4 步。

用户行本身**永远不 DELETE**：`groups.owner_uid`、`communities.owner_uid`、
`threads.created_by` 这几条外键没有 CASCADE，硬删用户行会把无关的域顶坏
（群没有群主、话题没有作者）。第 4 步做的是匿名化：
`name = 'deleted-<uid>'`、`avatar`/`signature` 清空、`pw_salt`/`pw_hash` 清零、
`pw_algo = 'deleted'`、`totp_secret = NULL`、`totp_enabled = 0`。

---

## 3. 回滚兼容说明

### 3.1 代码回滚（把 #114 这次提交 revert 掉）

**安全**，因为列可空且旧代码不读它：

* 旧 `verify_credentials` / `lookup_token` 的 SQL 不带 `deleted_at` 条件 →
  已经注销的账号在旧代码下会**重新可登录**（口令列已被匿名化清零，实际仍登不进来，
  但 `lookup_token` 不再过滤，若旧数据里有残留会话行就会重新认证成功）。
  ⇒ 回滚代码后必须先跑一次 `DELETE FROM sessions WHERE user_id IN
  (SELECT id FROM users WHERE deleted_at IS NOT NULL)`，或干脆不要回滚。
* 旧 `update_profile` 没有 `deleted_at IS NULL` 守卫 → 回滚后能对已注销账号改名。
  ⇒ 这是"回滚后行为变差"，不是"回滚会崩"。
* 旧 `JobStatus` 认不出 `cancelled` → 会折成 `Queued`（那正是本卡清掉的假绿）。
  回滚代码前请先确认库里没有 `cancelled` 行：
  `SELECT COUNT(*) FROM identity_job WHERE status = 'cancelled';`
  有就手工改回 `failed`，`error` 补一句 `cancelled before rollback`。

### 3.2 schema 回滚（把列去掉）

**不要做。** SQLite 的 `DROP COLUMN` 需要 3.35+，且这张表是热表；留着列的代价是
8 字节，去掉列的代价是一次全表重建。要"回滚注销"，回滚的是**数据**，不是 DDL：

```sql
-- 可回滚窗口：delete 作业还没跑（status 仍是 queued/failed）
UPDATE users SET deleted_at = NULL WHERE id = :uid;
-- 会话已经被撤销，用户需要用口令/恢复码重新登录；本卡不提供"复活会话"，
-- 因为那等于签发一张服务端没有记录过的凭证。
```

**不可回滚**的部分：作业一旦 `done`，逐域 DELETE 与匿名化已经提交，
昵称、签名、口令材料、关系、内容都没了。因此**注销没有"取消"接口**
（`preferences::cancel` 按种类拒绝 `delete`，HTTP 面回 409
`deletion cannot be cancelled`），`crates/platform/tests/account_surfaces.rs`
的 `cancel_stops_a_queued_export_and_never_undoes_a_deletion` 就是钉这条的。

### 3.3 分域处置（法定/业务必需记录）

`PURGE_DOMAINS` 八个域 + `preferences` 一档按域删除；**保留但脱敏**的两档写在
`PURGE_MASKS`：

| 表 | 处理 | 理由 |
|---|---|---|
| `ledger` | 保留行，清 `memo`/`counterparty`/`ref_id` | 账目是资金守恒证据，审计要能对平；删了别人账目说不清 |
| `redpackets` | 保留行，清 `memo`、`claimers='[]'` | 领取关系是**其他用户**账目的凭证 |
| `media`（字节本体） | 只删 `media` 元数据行 | 平台 `MediaStore` 接口没有 delete，不越界造一个；字节由媒体域自己的 GC 回收 |
| 消息信箱本体（`wxlike-message`） | 不在本卡删除范围 | 会话信箱按会话归属、不按人，越界删会破坏对方可读性 |

钱包余额非零时**整个清理被拒绝**（`wallet not empty`），因为静默销毁余额是不可
接受的"清理成功"；`delete_purge_refuses_a_non_empty_wallet_and_rolls_back` 断言
失败后一行都没动。

### 3.4 老库升级顺序

1. `Db::open` → `migrate_identity()`（G02-01，建 `identity_*` 表）
2. … `migrate_relations` / `migrate_g05` / `migrate_round5`
3. `migrate_account()`（本卡，补 `users.deleted_at`）

老行 `deleted_at = NULL` ⇒ 一律视为"未注销"，不需要回填。反过来，**任何新写的
代码路径都不许把 NULL 当成"未知"**：查询里显式写 `deleted_at IS NULL` /
`IS NOT NULL`，不写 `COALESCE(deleted_at, 0)` 之类的折中。

---

## 4. 请 G00 执行的三项（汇总）

1. `server/src/lib.rs` 起 `wxlike_platform::spawn_account_job_worker(platform.clone(),
   Duration::from_secs(30))`（`root_registration: worker_mount`）；
2. 把 `ALTER TABLE users ADD COLUMN deleted_at INTEGER;` 并入
   `schema/proposals/G02/identity.sql`，并在 `identity_job.status` 的说明里补
   `cancelled` 值域（**不加 CHECK**）；
3. 复核 `crates/platform/Cargo.toml` 新增的 `wxlike-security` 路径依赖与根
   `Cargo.lock` 的 1 行增量；决定是否为本卡写面新增 `risk::Action` 变体。
