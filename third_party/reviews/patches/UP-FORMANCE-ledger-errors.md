# UP-FORMANCE · `internal/domain/errors.go` → `crates/ledger/src/lib.rs`

- 上游：`formancehq/ledger` @ `fe463ef1f79adbd0701219bb65514b9f7dcd4a84`
  永久链接：<https://github.com/formancehq/ledger/blob/fe463ef1f79adbd0701219bb65514b9f7dcd4a84/internal/domain/errors.go>
- 许可：NOASSERTION（快照无 LICENSE 文件；`gh api repos/formancehq/ledger` 的 `license` 字段为 `null`。
  本项目只做内部研究、不对外分发，许可登记为信息位、不作阻挡）
- 形态：**错误分类改编**（Go 的 `SerializableError` + proto `FailureDetails` → Rust `thiserror` 枚举）

## 抄了哪段行为

新增的 `LedgerError` 变体逐个对应上游同名错误的**载荷形状**（带科目、带当前值、带幅度），
让"哪一格溢了""哪个资产写歪了"在错误里就能自证，而不是只有一句"失败"：

| 本仓库变体 | 上游对应 |
| --- | --- |
| `VolumeOverflow { account, current, amount }` | `errors.go:849-864` `ErrVolumeOverflow{Account, Asset, Color, Amount, Current, Side}` |
| `BadAsset(String)` | `validation.go:110` 的 `ErrAssetInvalid`（code `ASSET_INVALID`，委托外部 `invariants`） |
| `AssetMismatch { declared, journal }` | `validation.go:578` `ValidateAsset` 的"资产必须与本次记账同一格"意图 |
| `MetadataKey(String)` / `MetadataValue(String)` | `validation.go:536-576`：越界键名必须进错误（`metadataFactFailure.key`） |
| `MetadataEntryLimit` / `MetadataSizeLimit(usize)` | `metadata_limits.go` 的 `ErrMetadataLimitExceeded` 按维度区分（entries/key/value/entity/command） |
| `DuplicateMetadataKey(String)` | 上游 metadata 是 map 天然去重，本仓库要显式拒绝同键两次输入 |
| （既有）`InsufficientFunds(String, i64)` | `errors.go:817` `ErrInsufficientFunds{Account, Asset, Color, Amount, Balance}` —— **保持原签名**，既有验收用例 `matches!(.., InsufficientFunds(..))` 一字未改 |
| （既有）`IdempotencyConflict` | `errors.go:692-701` `ErrIdempotencyKeyConflict`（`Reason = IDEMPOTENCY_KEY_CONFLICT`） |

另外把 `ApplyOutcome`/`PostReport`（新再导出）作为"重放 vs 真入账"的回报类型，
对应 `internal/infra/state/machine.go:1528/1541` 的 `replayed` 分流。

## 改了什么

1. 上游每个错误都还要 `Kind() ErrorKind` + `Reason() string` + proto
   `FailureDetails()` 三件套（要进审计链、要跨 gRPC 传给客户端）；本仓库是进程内
   `thiserror`，只保留 Display 文案与结构化字段。
2. `ErrVolumeOverflow` 在 `i64` 世界里没有 `2^256` 的语义，字段收缩为
   `{account, current, amount}`（去掉 asset/color/side —— 我们没有 color 维度，
   side 在我们的净额算法里不等价）。
3. 错误文案不带密钥/SQL/连接串（沿用本 crate 既有纪律，见模块头注释）。

## 没抄什么

1. `FailureDetails` 的 protobuf 编码、`ReasonString` 与
   `commonpb.ErrorReason` 的映射表（`reason.go`）—— 我们没有 wire 层。
2. `ErrInsufficientFunds.ColorKnown`、`ErrBalanceNotPreloaded`（admission 预加载覆盖
   契约）、`ErrTransientAccountNonZero`（本快照里只有类型定义、grep 不到抛出点，
   详见 `UP-FORMANCE-ledger-entry-set.md`）都未移植。
3. 公共错误白名单机制（`public_errors.go`：哪些错误可以原样回给客户端）没有对应物，
   本仓库由 commerce 的 `map_ledger()` 做映射。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check`；
测试见 `crates/ledger/src/{model,store,schema}.rs` 的 `#[cfg(test)] mod tests`
与既有 `crates/ledger/tests/{conservation,immutability}.rs`（一字未改）。
