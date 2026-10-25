# G06 交付证据与待验清单

- 组：G06｜账本、支付、红包、卡券
- 分支/worktree：`feat/FP-G06` @ `D:/mimo/test/wxlike-g06`
- 合同：`commerce/v1`（`contracts/domains/commerce/`）
- 迁移提案：`schema/proposals/G06/0001_ledger_commerce.sql`（无编号，待 G00 分配）
- 集成请求：`integration-requests/G06/G06-01..05.yaml`

## 1. 本组实际交付

| 卡 | 实现文件 | 测试文件 |
|---|---|---|
| G06-01 | `crates/ledger/src/{lib,model,schema,store}.rs`、`crates/features/commerce/src/{accounts,store,schema}.rs` | `crates/ledger/tests/conservation.rs`、`tests/g06_01_accounts.rs` |
| G06-02 | `payments.rs`、`cards.rs`、`ports.rs`、`infrastructure/sandbox.rs` | `tests/g06_02_payments.rs` |
| G06-03 | `settlement.rs` | `tests/g06_03_settlement.rs` |
| G06-04 | `transfers.rs`、`redpackets.rs` | `tests/g06_04_transfers_redpackets.rs` |
| G06-05 | `entitlements.rs`、`orders.rs` | `tests/g06_05_entitlements_orders.rs` |
| 命令面 | `http.rs`（dispatch + webhook DTO） | 覆盖在各卡集成测试 |

账本模型：无余额列，余额由不可变 `ledger_entries` 聚合；`ledger_journals.business_key`
唯一；锁额在 `ledger_holds`；日结快照不可覆盖。金额全部为最小单位整数 + 币种。

## 2. 本地可复现结果（2026-10-24）

```text
cargo test -p wxlike-ledger -p wxlike-commerce
  commerce lib unit tests          8 passed
  g06_01_accounts                  5 passed
  g06_02_payments                  9 passed
  g06_03_settlement                7 passed
  g06_04_transfers_redpackets      9 passed
  g06_05_entitlements_orders       8 passed
  ledger lib unit tests            4 passed
  ledger conservation              4 passed
  合计 54 passed / 0 failed

cargo clippy -p wxlike-commerce --all-targets -- -D warnings   # exit 0
cargo fmt -p wxlike-commerce --check                           # exit 0
```

说明：只对 `wxlike-ledger` / `wxlike-commerce` 做局部构建与检查；根工作区全量构建与
依赖锁由 G00 串行执行（并行 worktree 共用 target 会互相覆盖）。

## 3. 验收项对应证据

| 验收要求 | 证据 |
|---|---|
| 并发扣款不透支、账本守恒 | `conservation.rs::hundred_concurrent_transfers_conserve_to_the_cent`（100 线程 × 97 分） |
| 崩溃重放守恒、同键不重复入账 | `conservation.rs::idempotent_journal_and_insufficient_funds`；各卡 `verify_books()` 断言 |
| available/locked 分离 | `g06_01_accounts.rs`、`g06_04_transfers_redpackets.rs`（冻结/解冻/捕获） |
| 日结与分录一致 | `crates/ledger` 内置日结测试 + `verify_closing_in`；commerce 每用例末 `verify_books()` |
| 跨币种拒绝隐式相加 | `type_total` 多币种拒绝；`g06_01_accounts.rs` |
| 伪造/重放/乱序回调 | `g06_02_payments.rs`（签名伪造、过期、event_id 重放、失败乱序无副作用） |
| 支付响应丢失仍一笔 | `lost_payment_response_recovers_via_webhook_without_double_posting` |
| 二维码换人/换金额失效 | `qr_ticket_is_bound_to_intent_amount_currency_and_payee` |
| 绑卡只存 token/掩码 | `cards.rs::tests::raw_row_never_contains_pan_or_cvv`（直接 dump 原始行） |
| 提现超时不重复出款 | `withdrawal_timeout_keeps_funds_held_then_recovers_without_double_pay` |
| 部分退款不超过可退额 | `partial_refunds_respect_the_cap_and_are_idempotent` |
| 对账差异可追溯、补账有审批 | `reconciliation_flags_differences_and_requires_approval_to_adjust` |
| 多用户抢最后一份无负数 | `red_packet_last_share_goes_to_exactly_one_claimant`（双线程） |
| 重试领取同一结果 | `red_packet_claims_are_idempotent_and_split_the_total` |
| 过期只退未领余额 | `expired_red_packet_refunds_only_the_unclaimed_part` |
| 重复核销只成功一次 | `unowned_entitlement_is_claimed_once_then_redeemed_once` |
| 离线核销明确要求联网 | `redemption_token_expires_and_offline_mode_is_refused` |
| 票证条码不含用户凭据 | `ticket_barcode_exposes_no_owner_or_credentials` |
| 支付成功 ≠ 交付成功 | `paying_an_order_does_not_fulfil_it` |
| 供应商超时不自动二次扣款 | `provider_timeout_leaves_the_order_unpaid_until_retry_succeeds` |

修复记录：过期路径曾在事务内改状态后直接 `return Err`，导致回滚；已改为先
`commit` 再返回 `Expired`（`transfers.rs`、`redpackets.rs`）。

## 3.1 质量闸门整改（PR 首轮 CI 后）

首轮 CI 的 quality job 报五项指标全部变差，按 `docs/code-quality-audit.md`
的修法整改（基线 `--update` 未动）：

- **Money/Currency 去重**：`wxlike-ledger` 不再自带一份 Money，改用共享内核
  `wxlike-types` 并再导出；`LedgerError` 保留原变体并加 `From<MoneyError>`。
  消掉 46 个跨文件重复窗口。
- **长函数拆分**：18 个 >60 行函数全部拆到 60 行以内（含两个 DDL 巨函数按
  域分组、对账匹配/落库分离、幂等重放抽 `store::replay`）。
- **魔法数字**：SQL 占位符改命名参数（`:id` 等）、列读取改列名访问、
  算法常量与 TTL 上限进 `const`；总量 162→157（低于基线）。
- **重复窗口** 66→53、**嵌套** 63 持平；`python scripts/quality-gate.py` 本地
  通过（41/10/63/53/157 对基线 41/10/63/66/162）。
- 语义保持：幂等重放仍"只回放现状、不再触碰供应商"（`insert_withdrawal` /
  `insert_refund` 返回是否新建），由原有超时恢复测试兜底。

## 4. 隐私与日志

- 卡号/CVV 只在 `bind_card` 调用栈内存在；原始行 dump 测试证明库内无 PAN/CVV。
- 卡幂等摘要为 SHA-256 十六进制；3 位 CVV 可能偶然出现在摘要中，因此测试按
  “不逐字存储 + 摘要形态”判定，而不是脆弱的子串匹配。
- 错误类型只暴露稳定分类，不携带供应商原始报文、SQL、连接串或密钥。
- 条码只含权益 ID 与短 HMAC，不含用户 ID、座位号或永久凭据。
- 回调日志要求（G00 挂载时执行）：不得记录 webhook secret、完整回调原文或卡数据。

## 5. 未完成 / 待验清单（不得据此关闭任务）

1. **真实支付供应商**：当前只有 `SandboxProvider`；真实通道的签名、对账单格式、
   超时语义需在集成环境实测。
2. **PostgreSQL 迁移**：提案 SQL 未在真实 PG 执行；G00 分配编号后需跑 expand 校验。
3. **HTTP 路由/鉴权**：`http.rs` 只有 dispatch；路由、原始 body 读取、actor 注入、
   admin/merchant audience 由 G00 与 G14 挂载并验收。
4. **真实群成员**：`StaticMembership` 仅测试用；生产必须接 G02 RelationsPort。
5. **G07 履约协作**：`order.fulfilled` 事件契约已给 fixture，但 G07 真实消费未合入。
6. **100 并发领取的红包压测**：本地只做双线程最后一份竞争；真实 HTTP 并发压测待集成环境。
7. **事件缺口**：拒收/过期/提现失败/对账开启等状态暂无事件（见 `events.yaml#not_yet_emitted`）。
8. **上游验收依赖**：G02-01 (#103)、G00-04 (#104)。未完成前各卡保持 OPEN。
