# UP-FORMANCE · `internal/domain/processing/processor_posting.go` → `crates/ledger/src/schema.rs`

- 上游：`formancehq/ledger` @ `fe463ef1f79adbd0701219bb65514b9f7dcd4a84`
  永久链接：<https://github.com/formancehq/ledger/blob/fe463ef1f79adbd0701219bb65514b9f7dcd4a84/internal/domain/processing/processor_posting.go>
- 许可：NOASSERTION（见 `UP-FORMANCE-ledger-asset-metadata.md`）
- 形态：**行为改编 + 数据库层再实现**（Go 的内存余额格 → SQLite 触发器）

## 抄了哪段行为

1. **"余额检查只豁免外部世界科目"** —— `:120`
   `if !skipBalanceCheck && posting.GetSource() != "world"`。
   我们的等价物是 `AccountType::is_asset()`：`wallet/escrow/clearing` 一律不许透支，
   `revenue/marketing/advertiser` 是"外部世界"，可以为负。这一条本来就在，
   本次只是把它的出处写进 `crates/ledger/src/model.rs` 的注释，避免后来人以为
   这是随手写的白名单。
2. **溢出即拒绝，绝不允许回绕** —— `:150-160` 与 `:182-192` 两处 `AddOverflow` +
   `ErrVolumeOverflow`，注释点名"plain uint256.Add wraps silently … money silently
   created or destroyed (#321)"。落成本仓库的三处：
   * `validate_lines` 的 `checked_add`（已有，加注释标明出处）；
   * `available_minor` 由 `booked - locked` 的裸减法改为 `checked_sub`，
     溢出回报新的 `LedgerError::VolumeOverflow { account, current, amount }`
     （错误载荷形状借 `internal/domain/errors.go:855` 的 `ErrVolumeOverflow`）；
   * `check_available` 用 `checked_neg` 取借记幅度，不再写 `-*amount`
     （`i64::MIN` 取负在 debug 构建下会 panic）。
3. **借贷平衡不靠一次性校验** —— 上游的平衡是"每笔 posting 必然一进一出"构造出来的
   （`applyPosting` 同时抬 source 的 output 与 dest 的 input）。我们没有 posting 对，
   只有分录数组，所以把等价的底线搬到数据库：journal 在插入时就声明 `entry_count`，
   `trg_ledger_entries_no_extra_row` 拒绝任何让既有 journal 的分录数超过声明值的插入。
   没有这条，"配平"就只是 `post_in` 那一刻的一次性检查，事后补一条分录就能把
   历史账做歪，而 `verify_journals` 要到下一次核对才发现。

## 改了什么

1. 三列新增：`ledger_journals.asset`、`.metadata`、`.entry_count`（都带
   `NOT NULL DEFAULT`），配 `migrate_journal_columns` 的 `ALTER TABLE` 升级路径；
   `SCHEMA_VERSION` 从 1 升到 2。
2. **旧行的 `entry_count` 保持 0，不回填**。除了语义上"0 = 分录集合已冻结"正好是
   我们要的，机制上也必须如此：老库文件里 `trg_ledger_journals_no_update` 已经存在，
   任何 `UPDATE ledger_journals SET entry_count = …` 的写法会被它当场打死，
   结果是升级后的库直接打不开。`upgrading_a_v1_database_adds_columns_without_a_backfill_update`
   这条用例就是专门钉这个的。
3. PostgreSQL 侧对应写法在 `schema/proposals/G06/0001_ledger_commerce.sql`
   （`trg_ledger_entries_frozen_set`）。

## 没抄什么

1. `internal/infra/state/write_set_ephemeral_purge.go:60-140` 的
   **transient/ephemeral 账户"期末必须归零否则不持久化"** 语义没有移植：本仓库的
   `escrow` 科目在红包场景下**必然跨多笔 journal 持有余额**（先充值进托管、
   再逐份领取），"每笔 journal 内托管归零"会让
   `g06_04_transfers_redpackets.rs::red_packet_claims_are_idempotent_and_split_the_total`
   这类既有验收失败。上游另有 `ErrTransientAccountNonZero`
   （`internal/domain/errors.go:1372-1391`）——**该错误类型在本快照里有定义，
   但我 grep 遍 `internal/` 没有找到 apply 路径上的抛出点**（只有
   `failure_details_producers.go`/`reason.go` 的登记），故不以其为依据编造规则。
2. `assetCache`/`cachedVolumeKey` 的性能结构、`uint256`/`big.Int` 大数、
   `skipBalanceCheck`（Force 交易）都没有搬：我们是 i64 最小单位 + 不允许强制透支。
3. Pebble 的 volume 持久化分层（kept/purged/transient）没有对应物。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check`；
测试：`crates/ledger/src/schema.rs::tests`（新增升级用例 + 原有幂等建表用例）、
`crates/ledger/src/store.rs::tests::appending_an_extra_entry_to_a_journal_is_refused_by_the_database`，
以及既有 `crates/ledger/tests/immutability.rs`（一字未改）。
