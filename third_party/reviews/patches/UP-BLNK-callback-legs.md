# UP-BLNK · `transaction_inflight.go` → `crates/features/commerce/src/payments.rs`

- 上游：`blnkfinance/blnk` @ `cb48a92f0afe7b5b2ba8d876d4f326e9f645b13e`
  永久链接：<https://github.com/blnkfinance/blnk/blob/cb48a92f0afe7b5b2ba8d876d4f326e9f645b13e/transaction_inflight.go>
- 许可：Apache-2.0（本地 `LICENSE.md`；`gh api repos/blnkfinance/blnk` → `Apache-2.0`）
- 形态：**行为改编的落地点**（判定逻辑在 `store::replay_legs`，这里是把它接进真实流程）

## 抄了哪段行为

`handle_provider_callback` 原来只看一条幂等腿（`commerce_provider_callbacks`
按 `callback:<operation_id>` 的 `request_digest`）。现在按 Blnk 的多腿判定重做：

1. **主腿**＝回调台账行的摘要（＝上游按 `actionID` 派生的 `reference`）；
2. **副腿**＝事件表 `commerce_payment_events.payload_digest`（＝上游"这条腿
   对应的子交易行"），新函数 `stored_event_digest()` 只读摘要；
3. 两条腿都按下发摘要落库，才认定为重放并原样返回首次结果；
   缺任何一条腿都不算完成，继续走首次执行路径
   （对应 `QueueInflightAction:912-920` 拿到 nil 才排队的写法）。

`findInflightActionByRef:704-712` 的"父单/状态/金额都要一致，否则键算被占用"
在本仓库由两处承担：主腿摘要不符 → `IdempotencyConflict`；
副腿状态不符 → 事件表自己的 `Replay` 拒绝。

## 改了什么

1. 判定入口从"命中就 return"改为"两条腿都算完再决定"，`replayed` 的对外语义不变
   （首次 false、重放 true），`mark_replayed()` 路径不变；
2. 重放分支里 `store::replay()` 只是把已读出的台账行取回值，
   若它反而给 None 就报 `BadInput("callback ledger row vanished")`——
   这是"同一事务内读到自己刚判定的行"的一致性断言，不是新行为；
3. 时间窗仍只约束首次执行（既有注释与用例保持不变）。

## 没抄什么

1. **不借金额宽松匹配**（`transaction_inflight.go:708` 的 `amount == nil ||
   amount.Sign() == 0` 视为与既有金额相同）。见 `UP-BLNK-replay-legs.md` 同条。
2. **不借 bulk 的"无批级幂等键"**（`BulkInflightUpdate:1024-1029` 明确
   "bulk has no batch-wide idempotency key；重试会看到 ALREADY_COMMITTED/
   ALREADY_VOIDED，调用方自己当成成功"）。本仓库的批量入口
   （红包批量领取、批量退款）仍然一次业务键一次效果，不把"已经是终态"
   折算成成功。
3. 上游的 asynq 排队、`StatusApplied/StatusVoid` 终态机、`collectInflightLegs`
   分页扫腿都没有对应物，未搬。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check`；
测试：既有 `crates/features/commerce/tests/g06_06_provider_callback.rs`
全部 15 条一字未改（尤其 `identical_callback_replay_is_single_effect`、
`replay_under_same_operation_id_with_changed_parameters_is_rejected`、
`same_event_under_a_new_operation_id_is_refused_by_event_dedup`、
`concurrent_identical_callbacks_have_exactly_one_effect`、
`crash_between_delivery_and_retry_does_not_double_post`）。
