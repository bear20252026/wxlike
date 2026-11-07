# UP-TINODE · `server/auth/code/auth_code.go` → `crates/features/identity/src/throttle.rs`

@@HEAD@@
- 次要来源：`server/validate/email/validate.go:350-380`（`Check` 里的 `Retries > MaxRetries`）

## 抄了哪个 upstream_path 的哪些行为

`auth_code.go` 的短期凭证状态机（89-170 行）：

1. **计数与凭据同一条记录**：PCache 的值就是 `"code:count:uid"`，键是 `code_<method>:<value>`
   ——即"重试次数属于**这个凭据**，不属于这一枚码" → `identity_retry` + `key_credential(method, value)`；
2. **先判上限、再比码**（`117-119`：`if count >= ca.maxRetries {{ return ErrFailed }}`，
   比对在 121 行）→ [`check_budget`] 的文档与调用位置（`login_challenge::verify` 里也在比对之前）；
3. **只有失配才 +1**（`121-125`：`parts[0] != code` 时才 `Upsert(count+1)`）
   → [`spend_failure`] 的名字与语义就是这个；成功路径一次都不计数；
4. **成功即删**（`128`：`store.PCache.Delete(key)`）→ [`clear_budget`]；
5. **下发时顺手 GC**（`144`：`PCache.Expire(realName+"_", now-lifetime)`）
   → [`sweep`]，调用点在 `login_challenge::issue`；
6. `config.MaxRetries < 1 → 初始化报错`、`ExpireIn <= 0 → 报错`（51-61 行）
   → `Budget` 由常量给出 + `check_budget` 对 `max_retries <= 0` 天然封死；
7. 上游两处的**上限口径本来不同**，都保留了：`auth_code` 的 `count >= maxRetries`
   与 `validate/email` 的 `cred.Retries > MaxRetries` → `Cap::AtCap` / `Cap::AfterCap`，
   并有 `after_cap_gives_one_more_try_than_at_cap` 钉住，防止以后被"顺手统一"。

## Rust 化改了什么

- 上游用 `PCache`（Redis/内存）存一个冒号分隔的字符串；本平台一律走 SQLite 表，
  计数、寿命、更新时间三列显式化，不再靠字符串切分（`strings.Split(value, ":")` 的
  `len(parts) != 3 → ErrInternal` 这类"格式即协议"的脆弱点直接消失）；
- 与既有固定窗口桶**并存且分工明确**（文件头那张对照表）：窗口桶管"多久能再发一次"，
  预算桶管"这一枚凭据一共能被试错几次"。上游只有后者，
  因为它的下发限速在 SMTP/短信网关那一侧，本平台没有那道天然闸门；
- `check_budget` 在寿命到期时**当场删行**再放行（上游靠 `Expire` 的 TTL 与显式 GC）；
- 失败一律回 `RateLimited {{ retry_after_ms }}`（上游是 `ErrFailed`，与"码错了"同形）：
  本平台 `retry_after_ms` 取"预算剩余寿命"，因为锁是随凭据死的，没有"再等 30 秒就能试"这回事。

## 没抄什么

- **重发即归零**这一条**故意不抄**：上游 `GenSecret` 每次都 `Upsert(code + ":0:" + uid)`，
  即重新下发会把计数清零（它那一侧的 key 就是这枚码）。本平台 `resend()` 是客户端可反复触发的
  同一个入口，照抄等于"换一枚码又能从头试"。所以 [`arm_budget`] 有行只续期、不清零，
  预算上限也从"单枚 5 次"之外另加"同凭据累计 15 次"。这是**加严**，登记备查；
- `sanitizeKey`（把 `%` 换成 `/` 以免 SQL LIKE 出错）：本平台用参数化等值查询，没有 LIKE；
- Redis 后端的 TTL 语义与 `PCache.Get` 把 `ErrNotFound` 改写成 `ErrFailed` 的那一步
  （本平台的"没有行"就是"没试过"，不需要伪装成失败）；
- `auth_code` 的 `Features: auth.FeatureNoLogin` 与 `AuthLevel: LevelNone` 组合
  （临时认证不许坐会话）——那条语义落在了 `session::open_temp` + `refresh::ensure_not_temp_auth`。
