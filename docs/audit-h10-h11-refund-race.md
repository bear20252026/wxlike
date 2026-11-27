# H-10 / H-11：退款审批竞态与累计超额

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `014b31d`。

## H-10 退款审批 read-await-update 竞态

审计原文：`AUDIT-60895e18.md:196-201`。原 `approve_refund` 先读退款单、判定审批人，
再 `await` money port，最后无条件 `UPDATE ... WHERE id = ?`。两个并发审批可以同时通过
prepare 并各自调用一次上游出账；真实重复打款取决于外部端点，但 service 层竞态已确证。

### 修复

`crates/admin/src/commerce.rs`：

1. `approve_refund_prepare` 仍负责准入、读单和审批人判定，但不写库。
2. `reserve_refund` 在调用 money port 之前做 CAS 预占：

   ```sql
   UPDATE g14_refund
      SET state = ?7, approved_by = ?5, second_approver = ?6, updated_at = ?8
    WHERE id = ?1 AND state = ?2
      AND COALESCE(approved_by, '') = ?3
      AND COALESCE(second_approver, '') = ?4
   ```

   谓词绑定“读到的旧 state + 旧审批人”，只有 affected rows = 1 的审批能继续。
   预占目标是 `processing`（需要出账）或 `partially_approved`（双人审批第一票）。
3. CAS 失败返回 422「退款状态已被其他审批改变，请刷新后重试」，失败方不碰钱。
4. 出账成功后再做一次 `approve_refund_finish` CAS：`WHERE id = ?1 AND state = ?6`，
   `expected_state` 就是预占状态；affected rows 必须为 1，否则不落审批结果。
5. 出账失败由 `mark_refund_apply_failed` 把 `processing` 标成 `apply_failed`，不回退
   自动重试。上游可能已经扣款，是否重放交给带同一幂等键的人工复核。
6. `HttpMoneyPort::apply_refund` 发送 `Idempotency-Key: refund:<refund_id>`。
   新增 `Upstream::call_with_idempotency`，原 `call` 保持无键行为；键透传到
   `call_blocking` 的请求头。这样同一退款重试时上游可按同一个键去重。

### 证据

- `cargo test -p wxlike-admin --lib refund_reservation_is_a_compare_and_swap`
  - 第一票预占 `pending -> processing` 成功；
  - 第二个审批带不同审批人、同一旧 state 再预占返回 false；
  - 最终状态仍是第一票写入的 `processing`。
- `cargo test -p wxlike-admin --lib upstream::tests::idempotency_key_is_forwarded`
  - 本地 HTTP 探针收到的原始请求包含 `idempotency-key: refund:rf_1`。
- `cargo test -p wxlike-admin --lib`：125 passed。
- `cargo test -p wxlike-admin --test g14_04_commerce`：7 passed，含小额单人审批、
  超阈值双人审批、退款幂等和订单金额上限。

### 边界

审计验收里的“真实 sandbox 并发审批只打款一次”仍无法在本机证明：默认 server 没有
`/apply` 端点（`G14-04.yaml` 只是集成请求样例，默认 404），本机也没有外部退款
sandbox。当前关闭的是 service 层双审批竞态和上游幂等键传递；接入真实 provider 时，
需要 provider 遵守 `Idempotency-Key` 语义，并补一条真实并发审批的集成记录。

## H-11 多笔退款累计超过订单金额

审计原文：`AUDIT-60895e18.md:203-208`。原 `request_refund` 只校验单笔金额不超过订单
全额；不同 `idem` 可以创建多笔各自合法、合计超额的退款申请。

### 修复

`request_refund` 在同一把 SQLite 锁里先做幂等查询，再汇总该订单未结/已结退款：

```sql
SELECT COALESCE(SUM(amount_cents), 0) FROM g14_refund
 WHERE order_id = ?1
   AND state IN ('pending','partially_approved','processing','approved')
```

`already_refunded + amount_cents > order.amount_cents` 返回 422，不插入行。查询与插入
在同一临界区，两个不同 `idem` 的并发申请不能同时穿透单笔校验。`approved` 计入总额，
避免已出账退款被后续申请重复占用额度。

### 证据

`cargo test -p wxlike-admin --lib cumulative_refunds_cannot_exceed_the_order`：

- 10,000 分订单先申请 8,000，成功；
- 第二笔 3,000 返回错误，库内仍只有 1 行；
- 再申请 2,000 成功，累计 10,000。

`g14_04_commerce::refund_request_is_idempotent_and_capped_by_order_amount` 同时覆盖
HTTP 路径的单笔上限和同 `idem` 重放。

### 边界

当前上限按“订单金额”而不是“可退金额”计算；若订单已部分履约/有运费或优惠分摊，
需要业务侧先给出可退金额口径再替换 `order.amount_cents`。数据库没有额外的
“订单唯一退款申请”约束，因为一笔订单允许多次部分退款；原子性由汇总+插入同临界区
保证。
