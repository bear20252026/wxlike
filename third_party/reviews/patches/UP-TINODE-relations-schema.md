# UP-TINODE · `server/store/types/types.go` → `crates/features/relations/src/lib.rs`

@@HEAD@@

## 抄了哪个 upstream_path 的哪些行为

`types.go` 里 `Subscription` 结构（893 行起）与 `DefaultAccess`（851 行）的**字段选择**：

1. `Subscription.ModeWant AccessMode` / `Subscription.ModeGiven AccessMode`
   —— "每一对订阅两列位"，对应新增表 `friend_acs(user_id, peer_id, mode_want, mode_given, updated_at)`
   与 `group_member_acs(group_id, user_id, mode_want, mode_given, updated_at)`；
2. `Topic.AccessAuth` / `Topic.AccessAnon`（+ `DefaultAccess{{Auth, Anon}}`）
   —— "每个被访问对象两列默认位"，对应 `friend_policy(user_id, access_auth, access_anon, updated_at)`；
3. 两列的**缺省语义**照上游：空 = 没说过（`UNSET`），与"明确拒绝"（`N`）是两回事，
   所以列默认值是 `''` 而不是 `'N'`，也不是 `'JRWPASDO'`。

## Rust 化改了什么

- 上游是数据库无关的 struct + 各家 adapter 的映射（`server/db/postgres|mysql|...`）；
  这里只有 SQLite DDL，写进 crate 自己的 `SCHEMA` 常量，沿用本文件既有的
  `CREATE TABLE IF NOT EXISTS` 幂等风格，G00 那边若要正式迁移，编号与建表语句照本文件抄；
- 表名不带 `tin_` 前缀，字段名沿用本平台既有的 `user_id/peer_id/created_at` 命名口径。

## 没抄什么

- `Subscription` 的其余列（`ReadSeq`/`ClearSeq`/`TouchedAt`/`DisabledAt`/`DeletedAt`/`State`）：
  未读游标与已读回执属 G03，本 crate 已经有 `group_meta`/`relations_audit` 各管各的；
- `DefaultAccess` 的 `json.Marshal` 存储（`Scan`/`Value`）：本平台把 auth/anon 存成两个字母串列，
  可 `SELECT ... WHERE access_anon LIKE '%S%'` 直接查，不必解 JSON；
- 老库补列的 `ALTER`（`ensure_friends_starred` 那种）：**这三张是新表，没有存量行需要补列**，
  所以没有迁移分支——这一点是刻意的，别照着 `friends.starred` 加一条假的补列逻辑。
