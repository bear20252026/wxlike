# H-15：偏好同 key 更新不推进 seq

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `1e55d56`。

审计原文：`AUDIT-60895e18.md:231-236`。`identity_pref` 的 `seq` 是自增主键，
`patch()` 用 `ON CONFLICT(user_id,key) DO UPDATE` 只改 value/updated_at/device_id。
同一 key 第一次写创建 seq，之后更新不产生新 seq，另一台设备按 `seq > since_seq`
增量拉取时永远看不到第二次变化。

## 修复

`crates/features/identity/src/preferences.rs::patch_with` 为每个补丁键显式分配新 seq：

1. 先插入一条临时分配行，让数据库自增分配新 seq：

   ```sql
   INSERT INTO identity_pref(user_id, key, value, updated_at, device_id)
   VALUES(?1, ?2, '', ?3, '')
   ```

   `key` 是 `__seq_probe_<random>`。`SqlExec::insert_returning` 在 SQLite 走
   `last_insert_rowid()`，在 PostgreSQL 追加 `RETURNING seq`。
2. 删除临时分配行。
3. 用新 seq 对目标 `(user_id,key)` 做 upsert：

   ```sql
   INSERT INTO identity_pref(seq, user_id, key, value, updated_at, device_id)
   VALUES(?1, ?2, ?3, ?4, ?5, ?6)
   ON CONFLICT(user_id, key) DO UPDATE SET
     seq = excluded.seq, value = excluded.value,
     updated_at = excluded.updated_at, device_id = excluded.device_id
   ```

为什么不直接 `seq = MAX(seq)+1`：两个并发更新会算出同一个值，撞主键。临时分配行
让数据库自己的自增序列分配值，SQLite 的 `sqlite_sequence` 和 PostgreSQL 的 identity
序列都会推进；删除临时行不会回退序列，后续插入仍从新值之后继续。临时行在同一事务
内创建和删除，外部读不到。

表结构不需要迁移，110 表 census 不变。

## 证据

`cargo test -p wxlike-identity --test preferences_jobs same_key_update_advances_seq_and_reaches_another_device`：

- `notify.dm=off` 首次写，游标为 1；
- 另一台设备把同 key 改成 `on`，`head_seq` 增大；
- 从旧游标 `since` 拉到 1 行：`notify.dm=on`、`device_id=dev-b`、`seq` 等于新 head；
- `identity_pref` 中该用户仍只有 1 行，没有残留 `__seq_probe_*`。

`cargo test -p wxlike-identity --test preferences_jobs` 全量 7 passed。

## 边界

本机没有 PostgreSQL，未跑 PG 并发更新；SQL 走同一 `SqlExec` 端口，PG 侧依赖
`insert_returning` 的 `RETURNING seq`。接入 PG fixture 后应补一条两个连接并发更新
同 key 的测试，断言两者得到不同 seq 且最终 `head_seq` 单调递增。
