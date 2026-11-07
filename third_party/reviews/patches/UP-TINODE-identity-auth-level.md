# UP-TINODE · `server/auth/auth.go` → `crates/features/identity/src/session.rs`

@@HEAD@@
- 次要来源：`server/auth/token/auth_token.go`（`GenSecret` 的 lifetime 三态、过期判定）、
  `server/session.go:920-995`（`login` 里的 user state 检查）

## 抄了哪个 upstream_path 的哪些行为

1. `auth.go:13-104` 的 `Level`（`LevelNone/LevelAnon/LevelAuth/LevelRoot`，`iota*10`）
   → `AuthLevel` 四个变体 + `as_str`/`parse`，**顺序即强弱**（上游用 `>` 比较，
   这里用 `PartialOrd`）；`MarshalText` 里 `LevelNone → ""` 也照抄；
2. `auth.go:106-180` 的 `Feature` 位图：`FeatureValidated`(`V`) / `FeatureNoLogin`(`L`)，
   含 `MarshalText`/`UnmarshalText` 的字母表与"表外字符报错" → `Features`；
3. `auth.go:207-229` 的 `Rec` 字段选择：`Uid/AuthLevel/Lifetime/Features/State/Credential`
   → `TokenRecord`（本平台只取签发侧需要的四个：级别、特性位、序列号、到期）；
   `Rec.Lifetime = time.Until(expires)` → `SessionGrant::time_left(now)`，
   并且照上游"负寿命 = 这枚不该存在"的态度，读侧夹到 0；
4. `auth_token.go:144-167` `GenSecret` 的 lifetime 三态：`== 0` 用配置缺省、
   `< 0` 直接 `ErrExpired` → `open_with` 开头那个 `match ttl_ms`；
5. `auth_token.go:26-39` `tokenLayout.SerialNumber` 的注释 *"to invalidate all tokens if needed"*
   → `current_serial` / `rotate_serial`：一次轮转作废之前签发的全部会话；
6. `session.go:965-975`：认证回来后还要看账号状态，非 OK 一律 `ErrPermissionDenied`
   —— 本平台对应 `require_active_device`（设备被吊销即会话不可用），位置一致。

## Rust 化改了什么

- 上游 `Rec` 是一次认证的**临时对象**（随请求生灭）；本平台没有跨进程的 auth handler 插件层，
  所以把它的字段落到一张本 crate 自有的侧车表 `identity_session_token`，
  **不动平台的 `sessions` 表**（那张表由平台建，加列等于改别人的合同）；
- `parse_level` 比上游 `ParseAuthLevel` **严格**：上游解析的是配置文件里写死的三个词，
  未知值回 `LevelNone` 无害；这里解析的是库里的一行，
  `"roo"` 折成 `None` 与折成 `Root` 之间差的是"少一个字母能不能提权"，所以回 `Corrupt`；
- `Features::parse` 同理：未知字母 → `IdError::Corrupt`（上游 `UnmarshalText` 返回 error，
  但把 `""` 与错值都当 0，这里不折）；
- 序列号不符的错误串与"令牌不存在"**完全同形**（`invalid or expired token`），
  照上游 `ErrFailed` 的处置，避免鉴权错误变成轮转状态探测仪。

## 没抄什么

- `auth.go` 的 `AuthHandler` trait（11 个方法的插件接口）：本平台不做可插拔认证器，
  一个 HMAC 票据 + 库里两枚令牌就够，抄接口只会引出一层没有第二个实现的抽象；
- `auth.Duration` 的 JSON 双形态解析（"5000s" 或纯秒）；
- `Rec.Tags / DefAcs / Public / Private`（注册期建号用的那几列，属账号域）；
- 上游把访问级别当权限位联合使用的 `accessFor`（本平台受众只有 User/Admin 两面，
  映射见 `AuthLevel::required_for`，管理面 = `Root`）；
- `session.go` 里 long-poll/websocket 的会话注册表（`sessionstore.go`），与本平台 HTTP 形态无关。
