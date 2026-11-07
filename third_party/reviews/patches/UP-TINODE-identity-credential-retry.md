# UP-TINODE · `server/validate/email/validate.go` → `crates/features/identity/src/login_challenge.rs`

@@HEAD@@
- 次要来源：`server/auth/code/auth_code.go:89-170`、`server/store/types/types.go:867-882`（`Credential`）

## 抄了哪个 upstream_path 的哪些行为

1. `validate.go:350-382` 的 `Check`（校验一枚短期凭证响应）的**四步顺序**：
   取活动凭据 → `Retries > MaxRetries → ErrPolicy` → 空响应 → `ErrCredentials` →
   比对（连 debug 值一起比）→ 命中 `ConfirmCred` / 失配 `FailCred`
   → 本文件 `verify` 现在就是这个顺序：**上限先判 → 预算先判 → 才比对 → 只在失配时计数**；
2. `types.Credential{Resp, Done, Retries}`（`types.go:867`）三列的含义
   → `identity_challenge` 的 `code_hash / consumed_at / attempts`，
   其中 `Retries` 的"随凭据走、成功即终止"语义由 `throttle::identity_retry` 承担；
3. `auth_code.go:121-131`：**失配才自增、成功即删**（原实现是"进门先 bump 再比对"，
   输对了也白吃一格配额——这次一并改掉，见 `attempts_are_counted_only_on_failure`）；
4. `auth_code.go:143-144`：`GenSecret` 里的 `PCache.Expire(prefix, now-lifetime)`
   → `issue` 里的 `throttle::sweep` + `purge_expired_rows`（下发时顺手扫，不起后台任务）；
5. `auth_code.go:98-104`：**"没有这条记录"与"码错了"回同一个错**（`ErrNotFound` 被改写成
   `ErrFailed`）→ 本文件既有的 `Unauthorized("invalid or expired code")` 同形纪律，
   新增的预算分支也**不**给出新错误类别可枚举的线索（`RateLimited` 只透露"你自己试太多"）。

"一个凭据只有一把活动码"也是从这条来的：上游 key 是 `code_<method>:<value>`，
新码 `Upsert` 就顶掉旧码。本平台 `identity_challenge` 一行一码，
所以补了 [`retire_siblings`]：同一 `phone + purpose` 下验证成功时把其它未消费挑战一并作废，
否则"下发 20 枚、猜中任意一枚"是免费的多次尝试。

## Rust 化改了什么

- 上游比对的是**明文** `cred.Resp == resp`（还要额外比一个 DebugResponse）；
  本平台库里只有 HMAC-SHA256(pepper) 摘要，比对走 `code_hash` + `ct_eq`，
  验证码不落明文（这一条是本平台既有的，比上游强，写清楚免得被当成没对上）；
- `FailCred` / `ConfirmCred` 的两次独立 DB 往返 → 消费 + 连坐放在**同一个事务**里，
  靠条件 UPDATE（`consumed_at IS NULL`）保证并发下只有一个赢家；
- `MaxRetries` 来自配置文件（缺省 3）→ `DEFAULT_MAX_ATTEMPTS`（5，单枚）
  + `CODE_BUDGET.max_retries`（15，同凭据累计），两级各有各的单元测试。

## 没抄什么

- 邮件模板、i18n 匹配、SMTP 发送（`v.send`、`ResetSecret`、`validationTempl`）：
  发信属平台网关层，本 crate 只把明文交给调用方；
- `token` 参数里的 URL 签名（`url.QueryEscape(string(token))`，点链接即验证的那种）：
  本平台走短信验证码，不做链接式验证；
- `hpCheck` / honeypot、`ResendInSec` 的 SMTP 侧节流（本平台用 `throttle::SMS_PHONE` +
  `resend_after` 表达同一意图）；
- `TempAuthScheme() → "code"` 与 `validate.Validator` 接口本体；
- `cred.Retries > MaxRetries` 的**具体数值**与"空响应单独回 ErrCredentials"这条分支：
  前者由本平台自定（5/15），后者会把"没填"与"填错"区分开，属于可枚举信号，故意合流。
