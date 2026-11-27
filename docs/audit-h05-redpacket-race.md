# H-05 红包领取竞态：行锁 + 条件更新

审计基线 `60895e18`，条目 H-05 指向 `crates/platform/src/domain/repo/wallet.rs::update_claim`：无条件 `UPDATE ... claimed = claimed + 1 WHERE id = ?`，没有 open/expiry/claimed < count 谓词，没有领取人唯一约束，也没有 affected-row 检查。PostgreSQL READ COMMITTED 下两个并发领取可以同时通过读快照，造成超领、超额。

## 修复

1. **行锁串行化同一红包的领取**
   `redpacket_state(conn, id, for_update)` 在 PostgreSQL 事务内生成 `SELECT ... FROM redpackets WHERE id = ?1 FOR UPDATE`，在读取状态前锁住红包行；SQLite 继续依赖单写锁，不生成 `FOR UPDATE`。
   `claim_redpacket_tx` 是唯一资金事务边界，因此后续所有校验、分钱、`update_claim` 都在锁内完成。并发领取同一红包时，后者会等前者提交，再看到新的 `claimed` / `claimers` / `status`。

2. **条件 UPDATE 作为最后一道闸**
   ```sql
   UPDATE redpackets
      SET claimed = claimed + 1,
          claimed_total = claimed_total + ?1,
          claimers = ?2,
          status = CASE WHEN claimed + 1 >= count THEN 'finished' ELSE status END
    WHERE id = ?3
      AND status = 'open'
      AND claimed < count
      AND expire_at > ?4
   ```
   `update_claim` 返回受影响行数；`pay_claim` 要求恰好 1 行，否则返回 `Conflict("redpacket changed")`，由 `with_tx` 回滚此前的加钱与领取人写入。

3. **领取人唯一性**
   PG 迁移 `schema/proposals/G05/0003_content_pg_compat.sql` 新增 `redpacket_claims(redpacket_id, user_id, share, claimed_at)`，主键 `(redpacket_id, user_id)`。`pay_claim` 在动钱包前先 `INSERT ... ON CONFLICT DO NOTHING`，返回 0 即 `Conflict("already claimed")` 并回滚；同一用户的重复领取有数据库级唯一键兜底。
   `claimers` 列表仍保留作为红包行内的领取人快照，`claimable` 在行锁内先做可读性拒绝。SQLite 没有这张 PG 专表，继续依赖单写锁 + `claimers` 列表；110 表 census 不变（该表是 PG 目标侧新增的唯一记录表，不是 SQLite 源表）。

## 验证

- `cargo test -p wxlike-platform --lib`：73 passed、20 ignored。
- `cargo test -p wxlike-platform --test wallet`：22 passed。
- 新增 `conditional_claim_update_refuses_a_finished_packet`：领满后第二次条件更新返回 0。
- `cargo test -p wxlike-platform --test wallet`：22 passed，含 `five_claimers_three_shares...` 的 5 并发抢 3 份、金额守恒、`claimed=3`、`claimed_total=100`、`status=finished`。
- `cargo fmt --check` 通过。

## 边界

- `FOR UPDATE` 只对 PostgreSQL 生效；SQLite 的串行性来自连接互斥锁，不模拟行锁。
- 频道红包成员资格仍在事务外预判；直聊/群已在事务内复核（H-01）。
- 真实 PostgreSQL 并发回归仍依赖 `WXLIKE_TEST_PG_URL` 的 CI job；本地无 `psql`/PG 19.0，未在本机跑 PG 并发。
