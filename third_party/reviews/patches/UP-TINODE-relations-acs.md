# UP-TINODE · `server/store/types/types.go` → `crates/features/relations/src/port.rs`

@@HEAD@@
- 本地 sha256：见 `third_party/reviews/registry.json` 里本文件那一条（改文件必须同步更新）

## 抄了哪个 upstream_path 的哪些行为

`server/store/types/types.go` 第 533-848 行的 `AccessMode`，逐条对应：

1. **位值本身**：`ModeJoin=1<<0 … ModeOwner=1<<7`、`ModeNone=0`、`ModeUnset=1<<8`、
   `ModeInvalid=0x100000`、`ModeBitmask=0xFF`（`Acs::JOIN…OWNER / NONE / UNSET / INVALID / BITMASK`）；
2. **全部预设常量**及其数值：`ModeCPublic`(47) `ModeCMeFnd`(41) `ModeCSelf`(199)
   `ModeCFull`(255) `ModeCP2P`(31) `ModeCP2PD`(95) `ModeCAuth`(63) `ModeCReadOnly`(3)
   `ModeCChnWriter` `ModeCChnReader`(11) `ModeCAdmin`(144) `ModeCSharer`(176)
   —— 单测 `bit_values_and_letters_match_upstream` 把这些数字逐个钉死；
3. **字母表与序列化**：`MarshalText` 的 `JRWPASDO` 顺序、`NONE → "N"`、`UNSET → ""`；
   `ParseAcs` 的大小写兼容、`N` 不得与其它位并存、未知字符报错（不忽略）；
4. **比较与差分**：`BetterThan` / `BetterEqual` / `Delta`（`+W-PA` 形态）/
   `ApplyDelta`（上游注释里的 `JPRA.ApplyDelta(-PR+W) -> JWA` 就是本仓库的一条断言）/
   `ApplyMutation`（含 `+`/`-` 走增量，否则整串赋值）；
5. **全部 `Is*` 谓词**：`IsJoiner/IsReader/IsWriter/IsPresencer/IsApprover/IsSharer/IsAdmin/IsDeleter/IsOwner/IsZero/IsDefined/IsInvalid`；
6. `server/utils.go:228-266`：`selectAccessMode`（含 `default → ModeNone` 这条 fail-closed）
   与 `getDefaultAccess`（按话题类目给默认位）→ `select_access_mode` / `default_access`；
7. `server/auth/auth.go:13-49`：`LevelNone/Anon/Auth/Root` 的**顺序语义** → `RelationLevel`；
8. `types.DefaultAccess{Auth, Anon}` → `DefaultAcs{{auth, anon}}`（好友隐私规则的存储形状）。

## Rust 化改了什么

- `type AccessMode uint` + 包级函数 → `pub struct Acs(u32)` + inherent methods；
  位常量从 `1 << iota` 写成显式 `pub const JOIN: u32`（Rust 的 `iota` 只在 enum 里有）；
- `error` 返回值全部收敛到 `crate::RelError`，且 `Display` **不回吐出错字符**
  （上游 `AccessMode: invalid character 'X'` 会把用户输入带进日志）；
- Go 的 `&`/`|` 运算符 → 具名方法 `cumulative / union / only / with / without / masked`，
  避免 `Acs & Acs` 在 `UNSET`（0x100）参与时静默产出无意义位；
  `cumulative` 在任一侧未定义时返回 `UNSET`，而不是照 Go 那样把 0x100 一起与进去；
- `parse` 的结果按上游 `UnmarshalText` 的做法 `& ModeBitmask`（`ParseAcs` 本身保留 UNSET 标记位，
  这一步不能省，否则 `J` 会变成 `0x101`）；
- `apply_delta` 显式要求串首是 `+`/`-`：上游的扫描循环遇到 `"-P"` 这种无首符号串会
  静默把第一段当"加"，这里改成报错——把收窄权限读成放大权限是不可接受的；
- `INVALID` 保留但不再当错误码用（Rust 走 `Result`），只作"绝对不开权限"的哨兵。

## 没抄什么

- `sql.Scanner` / `driver.Valuer` / `MarshalJSON` / `UnmarshalJSON`：本平台权限位落库存
  `to_flags()` 的字母串（`friend_acs.mode_want` 等列），JSON 侧由 `contracts/` 的合同类型负责，
  不需要这一层；
- `String()` 出错返回空串的写法（用 `Display` + `Debug` 取代，`Debug` 里区分 unset/invalid）；
- 上游 `ModeCP2PD`、`ModeCSys` 两个预设没有对应的产品路径，常量抄了（数值钉在测试里）但无调用方；
- `AccessMode` 在集群节点间的序列化、以及 `topic.go` 里 `MsgAccessMode` 的 wire 形状。
