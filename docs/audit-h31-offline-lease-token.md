# H-31：offline lease 缺 ownership token

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `5e859bc`。

审计原文：`AUDIT-60895e18.md:356-361`。`ack`/`release` 只按 `id`
DELETE/UPDATE；lease 过期被新 claimant reclaim 后，旧 claimant 仍可删除或释放新行。

## 修复

`crates/net/src/offline.rs`：

- `ClaimedFrame { id, claim_token, frame }`；`claim` 每次生成 16 字节随机
  token（hex），同一次领取的所有行共用；30 秒 lease 过期后重新 claim 会生成
  新 token。
- `ack(id, claim_token)`：`DELETE ... WHERE id = ?1 AND claim_token = ?2`。
- `release(id, claim_token)`：`UPDATE ... SET claimed_until = 0, claim_token = ''`
  `WHERE id = ?1 AND claim_token = ?2`。
- `offline_inbox` 新增 `claim_token TEXT NOT NULL DEFAULT ''`；老库通过
  `pragma_table_info` 检测后 ALTER，和 `claimed_until` 同一迁移入口。
- 内存实现同样的 token 匹配；不匹配时 ack/release 都是 no-op。
- `drain` 用领取到的 token 逐条 ack。

`crates/net/src/delivery.rs`：

- `Pending` 增加 `claim_token`；新增 `with_claim_token`。
- inflight 窗口 overflow / 断连 release / `stored_ack` 退休 / 过期秘密帧丢弃
  都带 token。

`crates/net/src/mq_hub.rs`：

- `pending_of(store_id, claim_token, frame)`；`flush_batch`、
  `release_from`、`track_written`、`drop_parked`、`handle_stored_ack`、
  `release_inflight` 全部透传 token。
- `crates/net/tests/delivery_outbox.rs` 的 `FlakyOffline` 适配新 trait。

## 证据

`cargo test -p wxlike-net`：全部通过（含 8 个 offline 单测、3 个
`delivery_outbox`、`keepalive`/`tofu`/`typing_channel` 等）。

新增 `offline::tests::stale_claimant_cannot_ack_or_release_the_reclaimed_row`：

1. claim 一行拿到 `old.token`；
2. 手动把 `claimed_until` 归零，模拟 30 秒过期；
3. 再次 claim 拿到同 id、不同 token；
4. `old.token` 的 ack 和 release 都执行成功但 depth 仍为 1，且新 lease 未被
   提前释放；
5. 只有 `fresh.token` 的 ack 能删掉该行。

原有 `claim_does_not_delete_until_ack`、`sqlite_claim_and_release_round_trip`
继续覆盖 claim/ack/release 基本语义。

## 边界

token 只用于 lease ownership，不是消息内容的认证；能写 SQLite 的攻击者仍可
直接改行。它解决的是并发 claimant 的 ABA，不替代数据库访问控制。PG 实现
（如果后续把 offline 迁到 PG）需要同样的 `claim_token` 条件更新。
