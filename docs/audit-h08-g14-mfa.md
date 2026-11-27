# H-08 G14 写操作 MFA

审计基线 `60895e18`：`crates/admin/src/auth.rs` 的 `writer()` 只拒绝静态 Operator 令牌；`crates/admin/src/g14.rs::write_via` 走 `auth::require(role, resource, action)` 角色矩阵，却没有检查 `session.mfa`。旧 `/api/op` 路径在 `rbac::authorize` 里传了 `session.mfa`，两条路径契约不一致。

## 修复

`write_via` 在角色矩阵通过后增加统一 MFA 门：

```rust
if !caller.session.mfa {
    let err = AdminError::MfaRequired(
        "G14 write requires an MFA-verified session".into(),
    );
    record_denial(...);
    return Err(err);
}
```

顺序保持：先静态令牌拒绝 → 再角色矩阵 → 最后 MFA。这样 viewer 先得到“角色不够”，不会把原因误报成 MFA；角色够但没验码的 superadmin 得到 403 + “mfa required”，并写一条 `Denied` 审计。

## 验证

- 新增 `crates/admin/tests/g14_01_auth_audit.rs::g14_writes_require_an_mfa_verified_session`：
  - 无 TOTP 的 superadmin 登录得到 `mfa=false`；
  - `POST /api/g14/jobs/job-1/retry` 返回 403，错误文本含 `mfa`；
  - 同角色 `mfa=true` 的 root 不被 MFA 门拦住（业务层返回其他错误/成功）。
- `cargo test -p wxlike-admin --test g14_01_auth_audit`：9 passed。
- `cargo fmt --check` 通过。

## 边界

- 静态 Operator 令牌仍在 `writer()` 处拒绝，MFA 门只对登录会话生效。
- G14 所有写入口经 `write_caller → write_via`；新增 G14 写路由必须复用该入口，不能直接 `incoming.writer()`。
- 未改旧 `/api/op` 的 `rbac::authorize(session.mfa)`，两条路径都要求 MFA。
