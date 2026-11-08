# UP-BLNK · `transaction_inflight.go` → `crates/features/commerce/src/store.rs`

- 上游：`blnkfinance/blnk` @ `cb48a92f0afe7b5b2ba8d876d4f326e9f645b13e`
  永久链接：<https://github.com/blnkfinance/blnk/blob/cb48a92f0afe7b5b2ba8d876d4f326e9f645b13e/transaction_inflight.go>
- 许可：Apache-2.0（本地 `LICENSE.md` 第 1-3 行即 Apache License 2.0 正文；
  `gh api repos/blnkfinance/blnk` 的 `license.spdx_id` = `Apache-2.0`）
- 形态：**行为改编**（Go 多腿 inflight 动作 → Rust 幂等腿判定）

## 抄了哪段行为

1. **"全部腿都落了库才算重放"** —— `replayInflightAction`
   （`transaction_inflight.go:726-746`）：它对 parent 下每一条 leg 都要
   `findInflightActionByRef` 命中，**任何一条腿命中失败就 `return nil, err`**，
   注释写明"and nil when any leg is still pending"；调用方
   `QueueInflightAction`（`:905-920`）拿到 nil 才继续排队执行。
   → 落成本仓库的 `store::replay_legs()` + `store::Leg`：只有主腿与所有副腿
   都按下发摘要落库才判"重放"，缺腿一律"不是重放，继续把效果做完"。
   这条是**加强**：本仓库原来只看一条腿（回调台账行），台账行在、事件行不在
   的中间态会被当成"已完成"。
2. **"键被占但内容不同"要单独成错** —— `findInflightActionByRef:704-711`
   在父单、状态、金额任一不符时报 `reference %s has already been used`
   （`api/errors.go:230` 把它分类成 `ErrTxnDuplicateReference`）。
   → 主腿摘要不符照旧 `CommerceError::IdempotencyConflict`。
3. **两步式读回：先问键占没占，再取整行** —— `:695-699`
   `TransactionExistsByRef` → `GetTransactionByRef`。
   → `payments::stored_event_digest()` 只 SELECT 摘要、不读载荷；
   `wxlike_ledger::stored_request_digest()` 同理（账本侧）。
4. **摘要必须定宽可比** —— Blnk 用 `reference` 字符串本身做唯一键，
   比对是整串相等；本仓库比对的是 SHA-256 hex。落成为
   `is_canonical_digest()`：长度 64 且全小写十六进制，否则不参与"相等即放行"。

## 改了什么

1. **副腿摘要不符不报冲突**，只返回"不是重放"，把拒绝权交回它自己的去重闸门
   （事件表 → `CommerceError::Replay`）。这是有意的偏离：`g06_06_provider_callback.rs`
   的 `same_event_under_a_new_operation_id_is_refused_by_event_dedup`
   钉的是"换个 operation_id 重投同一 event_id 必须以 `Replay` 告终"，
   若副腿也判冲突就会把这条更精确的错误吃掉。Blnk 没有这个第二道闸门，
   所以它不需要这层让步。
2. `replay()` 保留原签名与原语义（同参命中返回既有结果 / 异参冲突 / 未命中放行），
   只多了一道摘要形状检查；`replay_legs()` 是新增的同级入口。
3. 键长上限维持本仓库的 200（比 Formance 的 256 更紧，见
   `UP-FORMANCE-ledger-idempotency.md` 的对应说明），只补了出处注释。

## 没抄什么

1. **Blnk 没有 HTTP `Idempotency-Key` 头**。任务要求"交叉核对 `idempotency-key`
   处理"，实测：`grep -rn "Idempotency-Key\|IdempotencyKey" --include=*.go` 在
   `cb48a92` 全仓**零命中**；它的幂等靠的是业务列 `transactions.reference` 上的
   唯一索引（`sql/1770611011.sql:17-19`，注释原文"Unique index on transaction
   reference to enforce idempotency"）。→ **此处无源**：不编造"Blnk 的
   Idempotency-Key 中间件"，只借它真实存在的 reference/多腿判定。
2. **不借它的宽松金额匹配** —— `:708`
   `sameAmount := action == InflightActionVoid || amount == nil || amount.Sign() == 0
   || amount.Cmp(byRef.PreciseAmount) == 0`，即"金额为 nil/0 视为与既有提交一致"。
   本仓库一律要求摘要逐字节相等，宁可在同一键上多拒一次也不多放一次。
3. **派生键格式没有改成 Blnk 的下划线形态** —— `inflightActionReference`
   （`:637-646`）用 `<legTxID>_<action>_<actionID>`；本仓库业务键是
   `kind:id:action` 冒号分层，改格式会动到既有验收用例里按 `business_key LIKE`
   数行数的断言（`g06_06_provider_callback.rs:31` 的 `journal_rows`）。
   只借"派生键必须完全确定，重试必然撞上同一个 UNIQUE"这条语义。
4. **记录形状里不能借的部分**（交叉核对结论）：
   * `model/balance.go:37-60` 的 `Balance` 有 `Version int64` 乐观锁列、
     `CreditBalance/DebitBalance/Inflight*/Queued*` 六档余额列和
     `InflightExpiresAt`；本仓库**没有可变余额行**（余额只由 `ledger_entries`
     求和导出），既没有可加锁的行也没有可过期的在途列 → 不可移植，未借。
   * `model/ledger.go:20-26` 的 `Ledger` 只有 `LedgerID/Name/MetaData`，
     与我们的 `ledger_accounts`（kind/currency/fund_class/owner）不是同一层概念。
   * `Balance` 的 `(indicator, currency)` 唯一性对应我们已有的
     账户名唯一 + `validate_lines` 的币种一致检查，无需新增。
5. `isTerminalInflightError`（`:651-661`）的"重试是否值得"分类没有对应物：
   本仓库不用 asynq/队列重投，同步返回结果。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check`；
测试：`crates/features/commerce/src/store.rs::tests`
（`replay_requires_a_canonical_digest`、`replay_legs_only_replays_when_every_leg_landed`），
外加既有 `crates/features/commerce/tests/g06_02_payments.rs`、
`g06_04_transfers_redpackets.rs`、`g06_06_provider_callback.rs`（一字未改）。
