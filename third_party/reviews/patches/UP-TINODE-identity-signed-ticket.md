# UP-TINODE · `server/auth/token/auth_token.go` → `crates/features/identity/src/refresh.rs`

@@HEAD@@

## 抄了哪个 upstream_path 的哪些行为

整份 `auth_token.go` 的 `tokenLayout` + `Init` + `GenSecret` + `Authenticate` 四件套：

1. `tokenLayout`（26-39 行）的**字段集合与含义**：`Uid / Expires / AuthLevel / SerialNumber / Features`
   → `Ticket` 的六个字段（外加本平台需要的 `generation`）；
2. `Init`（42-77 行）的两条启动期硬检查：
   `len(config.Key) < sha256.Size → "the key is missing or too short"`、
   `config.ExpireIn <= 0 → "invalid expiration value"` → `TicketSigner::new`
   （`MIN_TICKET_KEY_BYTES = 32`）；
3. `Authenticate`（95-141 行）的**分支顺序与错误类别**：
   - 太短 → `ErrMalformed`；
   - 签名不符 → `ErrFailed`；
   - 级别越界 → `ErrMalformed`；
   - 序列号不符 → `ErrFailed`（**与签名不符同类同形**，单测
     `forged_and_rotated_tickets_are_indistinguishable` 直接断言两条错误串相等）；
   - `expires.Before(now.Add(1s))` → `ErrExpired`（`EXPIRY_SKEW_SECS`）；
4. `GenSecret`（144-167 行）：`rec.Lifetime == 0 → 用配置缺省`、`< 0 → ErrExpired`、
   返回 `(token, expires)` → `TicketSigner::mint`；
5. `DelRecords` 对 token 认证器是 no-op（无状态，靠 serial 作废）
   → 本平台同样只提供 `rotate_serial`，不做逐枚吊销表。

同文件里另外两处是本平台的既有实现，未动：`rotate` 的轮换 + 重放检测、
`ensure_access_alive`。新增的 `ensure_not_temp_auth` 则借自
`auth.go:112-114` 的 `FeatureNoLogin` 注释（"不得用于永久认证一个会话"）：
带 `L` 的会话出现在刷新链上 → 拒轮换并作废整条链。

## Rust 化改了什么

- **没抄字节布局**（下面"没抄什么"第 1 条详述），改成长度前缀的规范化编码
  （与同 crate `login_challenge::code_hash` 同一套写法），签名仍是 HMAC-SHA256，
  仍是"载荷与签名同串"的自包含令牌（`hex(body).hex(hmac)`）；
- 解析全部走 `checked_add` / `try_from` + 越界即 `BadInput`：定长切片的
  `body[i..i+8]` 在任何一处省掉长度检查都会 panic，而这是网络输入；
- `hmac::Mac::verify_slice` 做常量时间比对（上游用 `hmac.Equal`，同一意图）；
- 错误类别映射：`ErrMalformed → IdError::BadInput`、`ErrFailed → Unauthorized`、
  `ErrExpired → Expired`，与平台既有的 HTTP 映射对齐；
- `level_code`（0/10/20/30）保留上游 `iota * 10` 的**留空编号**，
  将来插一档不会让老令牌的级别集体错位；未知字节解码成 `None` 后由白名单当场拒。

## 没抄什么

- `[8:UID][4:expires][2:authLevel][2:serial][2:features][32:signature]` 这个 **50 字节定长布局**：
  上游 `Uid` 是 `uint64`（XTEA 可逆映射）、`Expires` 是 `uint32` unix 秒；
  本平台的 `user_id`/`sid` 是变长字符串，照搬要先发明一套 id→uint64 映射，
  为了形似多出一层能出错的东西。位宽换来了"32 位秒计数 2106 年溢出"和
  "序列号只有 16 位"这两个约束，本平台用 `i64`；
- `serialNumber` 从配置文件读（上游只能重启改），本平台是库里的单行 + `rotate_serial()`；
- `types.ErrUnsupported` 的那 5 个方法（`AddRecord/UpdateRecord/IsUnique/AsTag/RestrictedTags`）
  ——本平台不做插件式认证器，没有对应接口要填；
- 上游"从 `time.Unix(int64(tl.Expires), 0).UTC()` 反推 Lifetime 填进 `auth.Rec`"
  这一步：`Ticket` 里直接留 `expires_at`，由调用方自己算剩余。
