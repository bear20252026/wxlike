# G06 迁移回滚说明

本提案只新增表与索引，不修改既有表，因此回滚优先“让旧二进制继续读 expand schema”，
而不是执行破坏性 down migration。

## 允许的回滚方式

1. **首选：停止新代码写入，保留表。** 旧二进制不认识 `commerce_*` / `ledger_*` 新表，
   忽略即可；资金事实与幂等键全部保留。
2. **放弃功能但保留数据：** 删除对应的路由/命令挂载（由 G00 在组合根执行），
   表与索引保持不动。
3. **彻底移除（仅限未承接真实资金的空库）：**
   ```sql
   DROP TABLE IF EXISTS commerce_outbox, commerce_redemptions, commerce_redemption_tokens,
       commerce_entitlements, commerce_entitlement_rules, commerce_redpacket_claims,
       commerce_redpacket_shares, commerce_redpackets, commerce_nonces,
       commerce_transfers, commerce_reconciliation_exceptions, commerce_refunds,
       commerce_withdrawals, commerce_topups, commerce_cards,
       commerce_provider_callbacks, commerce_payment_events,
       commerce_payment_intents, commerce_orders, commerce_meta CASCADE;
   DROP TABLE IF EXISTS ledger_closing_balances, ledger_closings, ledger_holds,
       ledger_entries, ledger_journals, ledger_accounts CASCADE;
   DROP FUNCTION IF EXISTS ledger_entries_immutable();
   DROP FUNCTION IF EXISTS ledger_journals_immutable();
   DROP FUNCTION IF EXISTS ledger_closings_immutable();
   ```
   一旦有任何真实支付/提现/退款，禁止执行本步骤。

## 兼容窗口说明（append-only 触发器）

`ledger_entries` / `ledger_journals` / `ledger_closings` / `ledger_closing_balances` 上的
`BEFORE UPDATE OR DELETE` 触发器只拒绝写入，不改变任何读路径，也不改列：

* 旧二进制只做 INSERT，因此装上触发器后**无需同步发版**，可以先 expand 触发器再上新代码；
* 反过来（先上代码后装触发器）也只放宽一次兜底，不影响正确性；
* SQLite 实现用 `CREATE TRIGGER IF NOT EXISTS`，在 `init_schema` 里幂等执行，
  旧库文件下次打开即获得同样兜底，不需要人工回填；
* 回滚触发器本身（`DROP TRIGGER`）是安全的，但会让"禁止直接改余额"退化为纯代码约束，
  需要与 G14 一起确认没有其它服务在写这几张表。

## 禁止的操作

- 删除或改写 `ledger_entries` 来“撤销”一笔支付；纠错只能追加反向分录。
- 清空 `ledger_journals.business_key` 后重建数据：会破坏幂等与跨域引用。
- 把外部供应商的 `provider_ref` 置空后重放同一业务键：可能导致重复出款。
- 用 `DROP COLUMN` 回收 `commerce_payment_events` / `commerce_reconciliation_exceptions`：
  审计链会断，需跨一个兼容窗口并由 G14 审批。

## 数据核对

回滚前必须跑：

```sql
-- 每币种分录总和为零
SELECT currency FROM ledger_entries GROUP BY currency HAVING SUM(amount_minor) <> 0;
-- 每笔 journal 借贷平衡（借方=贷方绝对值）
SELECT journal_id FROM ledger_entries GROUP BY journal_id
HAVING SUM(CASE WHEN amount_minor > 0 THEN amount_minor ELSE 0 END)
    <> -SUM(CASE WHEN amount_minor < 0 THEN amount_minor ELSE 0 END);
-- 未处理的对账差异
SELECT COUNT(*) FROM commerce_reconciliation_exceptions WHERE status = 'open';
```

两项分录检查都必须为零行；未处理差异必须先由 admin 给出审批引用。
