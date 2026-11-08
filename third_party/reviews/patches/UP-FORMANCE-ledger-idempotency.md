# UP-FORMANCE · `internal/infra/state/machine.go` → `crates/ledger/src/store.rs`

- 上游：`formancehq/ledger` @ `fe463ef1f79adbd0701219bb65514b9f7dcd4a84`
  永久链接：<https://github.com/formancehq/ledger/blob/fe463ef1f79adbd0701219bb65514b9f7dcd4a84/internal/infra/state/machine.go>
- 许可：NOASSERTION（快照与 GitHub API 都没有许可声明，见
  `UP-FORMANCE-ledger-asset-metadata.md` 同一节说明）
- 形态：**行为改编**（Go/FSM+Pebble → Rust/SQLite 单事务）

## 抄了哪段行为

`internal/infra/state/machine.go:1505-1560` 的幂等分流：

1. **先比哈希再决定要不要跑管道**：`:1514` `proposalHash = fsm.processor.HashProposal(proposal)`，
   `:1520` `bytes.Equal(proposalHash, stored.GetHash())`。我们落成为
   `post_checked_in` 的第一步：`posting_digest(req)` 命中 `ledger_journals.request_digest`
   就**立刻返回既有结果，不再跑 `validate_lines` / `check_available`**。
2. **同键异体 = 拒绝，不是重放**：`:1526` `err = &domain.ErrIdempotencyKeyConflict{Key}`
   且注释明确"conflict is a fresh rejection, not a replay"。→ `LedgerError::IdempotencyConflict`。
3. **重放要能被发现**：`:1528` / `:1541` 的 `replayed = true` 两条分支（成功重放、失败重放）。
   → 本仓库新增 `ApplyOutcome::{Applied, Replayed}` 与 `PostReport`，让"这次没有产生新行"
   成为可以直接断言的事实；`post_in` 保持原签名，是这个新函数的包装。
4. **摘要只需哈希、不需读整份载荷**：`internal/infra/state/idempotency_store.go`
   只按 key 取一个 `IdempotencyKeyValue`。→ 新增 `store::stored_request_digest()`
   （只 SELECT `request_digest`），供上层"键占没占"的判定用。

## 改了什么

1. 摘要输入扩到 `asset` 与 `metadata`：`posting_digest` 的片段顺序是
   kind、currency、memo、(asset)、(metadata…)、逐条 (account, amount)。
   **asset 未声明且 metadata 为空时不产生任何片段**，所以升级前后摘要逐字节相同
   ——老库的 `request_digest` 仍然能命中重放，不会把历史重试误判成参数变更
   （`digest_matches_the_pre_asset_format_when_undeclared` 钉住这条）。
2. `machine.go` 的哈希是 proposal 的 protobuf 规范化字节；我们是"长度前缀 SHA-256"
   （沿用本仓库既有 `digest_parts` 规范，与 commerce 的 `request_digest_hex` 同一套）。
3. 校验位置借 `internal/domain/processing/processor_transaction.go:344-366`
   （`validatePostings`：地址/资产/颜色在"幂等命中之后、写库之前"整批核一次）：
   `validate_lines` 现在先跑 `validate_asset`，再逐条核账户/币种/资金性质，最后核总额 0。

## 没抄什么（此处无源，或不可移植）

1. **失败结果记忆化（memoized failure）没有移植**。上游 `:1528-1540` 会把首次失败的
   `Failure` 连同哈希存进幂等表，重试同一 body 时**原样重放那次失败**（`ReplayedFailure`）。
   我们没有照做，原因是事务边界不同：本仓库的分录与 commerce 的业务记录在
   **同一个 SQLite 事务**里提交，任何"把失败也写成持久记录"的做法要么随回滚一起消失
   （等于没做），要么必须逃在调用方事务之外单独提交（破坏"全无或全有"这条底线，
   还会让"余额不足 → 充值后重试成功"这条既有验收变成永久拒绝）。
   → 判定：**此处不采用，不是没找到，而是采用即削弱既有保证**。
2. **幂等键 TTL 与驱逐没有移植**。上游 `IdempotencyExpiresAt`（`internal/infra/state/
   idempotency_store.go:90`）与 `IdempotencyEvictionScheduler` 让键可以过期后重用。
   本仓库的幂等记录就是 journal 本身，`business_key` 是永久 UNIQUE；引入过期等于
   放开"同键重复入账"的口子 → 不移植。
3. **HLC 单调时钟、raft index、audit hash chain** 都是它们的共识层机制，我们没有对应物。
4. `ErrIdempotencyKeyConflict` 的 `FailureDetails` 结构化错误载荷（proto）没有搬，
   我们只有 `thiserror` 枚举。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check`；
测试：`crates/ledger/src/store.rs` 的 `#[cfg(test)] mod tests`
（`replay_is_reported_separately_from_a_fresh_post`、
`asset_and_metadata_join_the_idempotency_digest`、
`stored_request_digest_reads_only_the_frozen_hash`）+ 既有
`crates/ledger/tests/conservation.rs::idempotent_journal_and_insufficient_funds`
（一字未改）与 `crates/features/commerce/tests/g06_06_provider_callback.rs`
（一字未改）。
