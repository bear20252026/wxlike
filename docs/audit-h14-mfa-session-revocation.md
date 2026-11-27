# H-14：TOTP disable/re-enrollment 不撤销旧 session/step-up

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `e9a4338`。

审计原文：`AUDIT-60895e18.md:224-229`。`totp_enroll`、`totp_confirm`、`totp_disable`
都会改变 MFA 边界，但只更新 `admin_accounts`，不撤销 `admin_sessions`，也不删
`g14_stepup`。旧 bearer 继续 authenticated，持久化的 step-up 跨 seed 轮换/降级存活。

## 修复

`crates/admin/src/accounts.rs`：

- 新增 `AccountStore::revoke_mfa_bound_state(tx, account_id)`：
  - 删除该账号全部 `admin_sessions`；
  - `g14_stepup` 表存在时删除该账号全部 step-up 记录。
- `totp_enroll`：更新 seed + `totp_enabled=0`、撤销会话/step-up，三步在同一个
  `unchecked_transaction` 内提交。
- `totp_confirm`：更新 `totp_enabled=1` 与撤销会话/step-up 同一事务。确认前拿到的
  `mfa=false` 会话不能借这次确认自动升级。
- `totp_disable`：清 secret + `totp_enabled=0` 与撤销会话/step-up 同一事务。
- `g14_stepup` 是 G14 迁移建的表，`AccountStore::open` 单独使用时可能还没有它；
  helper 先查 `sqlite_master`，没有表就只删会话，避免把“表不存在”误报成失败。

## 证据

`cargo test -p wxlike-admin --lib mfa_boundary_changes_revoke_sessions_and_stepups_atomically`：

- 账号先拿一个 `mfa=false` bearer，并手工建 `g14_stepup` 插一条；
- `totp_enroll` 后旧 bearer `session() == None`，step-up 计数 0；
- 用新 seed 确认并登录，再插 step-up；
- `totp_disable` 后第二个 bearer 也 `None`，step-up 计数 0。

`cargo test -p wxlike-admin --lib`：127 passed。
`cargo test -p wxlike-admin --test g14_01_auth_audit`：9 passed，含 MFA 写门与 step-up
前后审计。

## 边界

`totp_confirm` 会让当前会话失效，客户端确认 MFA 后必须重新登录；这是有意的安全
语义，但前端需要处理“确认成功 + 需要重新认证”的响应。真实多标签页/多设备并发
撤销未做浏览器级 E2E；事务保证数据库层原子性，HTTP 层只有单进程测试。
