# UP-FORMANCE · `internal/domain/{validation,metadata_limits}.go` → `crates/ledger/src/model.rs`

- 上游：`formancehq/ledger` @ `fe463ef1f79adbd0701219bb65514b9f7dcd4a84`
  永久链接：<https://github.com/formancehq/ledger/blob/fe463ef1f79adbd0701219bb65514b9f7dcd4a84/internal/domain/validation.go>
- 许可：本地快照与 GitHub API 均无许可声明（`gh api repos/formancehq/ledger` 的 `license` 为 `null`，仓内 `find -iname '*licen*'` 无命中）→ 按 **NOASSERTION** 登记。
  本项目只做内部研究、不对外分发（G00 决定：许可不作阻挡项），但要写清"许可未查明"。
- 本地 sha256：见 `third_party/reviews/registry.json` 同 path 条目
- 形态：**跨语言改编**（Go → Rust；不是逐行翻译，也没有 copy-paste）

## 抄了哪段行为

1. **typed asset 的规范形态** —— `internal/pkg/filterexpr/parser.go:809-827`（`splitAsset`）。
   上游注释是可核实的规则陈述：`ValidateAsset` 拒绝"畸形 base、非数字或超范围的精度、
   以及会指向另一个余额格的**非规范后缀**（`USD/0`、`USD/02`、`USD/2/3`）"；裸 base
   （无 `/`）精度为 0。本仓库落成为 `Asset::parse` + `Asset::canonical`：
   只接受 `BASE` 与 `BASE/2`，`CNY` 与 `CNY/2` 规范化成同一个串（否则同一笔钱会有两种摘要，
   重投会被误判成参数变更）。精度上限借 `internal/domain/validation.go:52-54`
   （`MaxHasAssetPrecision = math.MaxUint8`，精度存一个字节）。
2. **标识符的长度与字符集** —— `internal/domain/validation.go:88-91`（`maxColorLength = 32`）
   与 `:593`（`ValidateColor`：仅大写字母）。长度上限 32 直接沿用；字符集这一条
   我们用本地 `Currency::parse`（3 位大写字母）而不是上游的 `^[A-Z]*$`，见"没抄什么"。
3. **metadata 的调用形状** —— `internal/domain/validation.go:536-576`
   （`ValidateMetadataKey` 保留越界键名进错误、`ValidateMetadataValue` 对字符串值查
   NUL 字节）；本仓库落成为 `validate_metadata_key` / `validate_metadata_value`，
   错误变体 `LedgerError::MetadataKey/MetadataValue` 同样带键名。
4. **metadata 的大小合同** —— `internal/domain/metadata_limits.go:39-45` 的四个默认上限：
   128 条 / 键 256B / 值 16KiB / 单实体 64KiB，一一对应
   `MAX_METADATA_ENTRIES` / `MAX_METADATA_KEY_BYTES` / `MAX_METADATA_VALUE_BYTES`
   / `MAX_METADATA_TOTAL_BYTES`。

## 改了什么

1. `Asset` 是 Rust 结构体（`Currency` + `u8` 精度），没有 Go 的字符串即资产；
   `Metadata` 是"按键升序、键唯一"的规范集合（`Vec<(String,String)>`），
   因为摘要必须与插入顺序无关 —— 上游不需要这条（它的 metadata 是 map，
   哈希在 proposal 层做）。
2. **不引入序列化依赖**：`Metadata::encode/decode` 用长度前缀文本（`<条数>;<键长>:<键><值长>:<值>…`），
   解码对越界切片、前导零长度、非规范分隔一律回报 `BadStoredMetadata`，不 panic。
3. 键的字符集比上游更紧：除 NUL 之外还拒绝控制字符、空白与 `|`（`|` 是摘要片段分隔符）。

## 没抄什么（以及为什么）

1. **`invariants.ValidateAsset` 的正则本体**：`validation.go:578` 只是
   `wrapValidationErr(invariants.ValidateAsset(asset))`，真现在外部模块
   `github.com/formancehq/invariants` 里，本地快照与 `vendor/` 都没有（`ls vendor` 无命中），
   `go env GOMODCACHE` 也没缓存。所以 **base 的字符集规则此处无源**：我们沿用本地
   `wxlike-types::Currency::parse`（3 位大写字母），把"精度后缀必须规范"这条能从
   `splitAsset` 注释核实的行为落地。`Asset::parse` 的精度判定因此是自研实现，
   不是上游代码的翻译。
2. `ValidateAccountAddress`（`validation.go:484`）同样只是外部 `invariants` 的委托，
   账户地址规则不可核实 → **未移植**，账户名规则继续由本仓库
   `store::validate_account` + commerce 的 `wallet:{owner}:{currency}` 命名承担。
3. Numscript、`QueryFilter` 深度上限、prepared query、`ValidateColor` 的 color 维度
   （按 (account, asset, color) 分余额格）都**没有**搬：我们只有一个币种一个尺度，
   加 color 会平白多出一个没人用的余额维度。
4. metadata 的五档上限里只用了四档（entries/key/value/entity），
   `command` 那一档（1MiB 一条 Raft 命令）对我们没有对应物。

## 复核方式

`PYTHONPATH=tools/source-audit python -m sourceaudit provenance check` 核对 sha256；
测试在 `crates/ledger/src/model.rs` 的 `#[cfg(test)] mod tests`
（`asset_accepts_only_canonical_forms` / `metadata_is_canonicalised_and_bounded`
/ `metadata_encoding_roundtrips`）。
