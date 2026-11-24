# Platform 门面归属表

`Platform` 的公开方法已经压到 102 个，但“少”不等于“有结构”。这份表把每个
公开方法归到一个门面，作为后续拆 trait / 收敛组合根的边界。

## 归属

| 门面 | 模块 | 当前 pub 方法 |
|---|---|---|
| Account | `sessions` / `security` / `recovery` / `devices` | 12 |
| Messaging | `read` / `notification` | 5 |
| Media | `video_refs` | 1 |
| Wallet | `wallet` | 9 |
| Governance | `report` / `audit` / `secret` / `perms` / `trust` | 21 |
| Realtime | `call` / `live` | 24 |
| Social | `communities` / `friends` / `relations` / `favorites` / `status` / `groups_admin` | 30 |
| Health | 组合根自身（readiness） | 0 |

合计 **102**。

## 规则

1. 新公开方法必须能归入上表某个门面；归不进去说明边界没想清楚，先不要 `pub`。
2. 门面之间不互相调用；跨门面协作走 domain 层或显式端口，不经过 `Platform` 的
   另一个公开方法。
3. 门面内部实现优先 `pub(crate)`；只有真实跨 crate 入口才 `pub`。
4. 后续拆 trait 时按这张表逐个门面迁移，不按文件零散改签名。

## 门禁

`ci.yml` 的 `fmt` job 执行：

```bash
python3 scripts/platform_surface_audit.py --max-pub 120 --check-facades
```

未归类的公开方法、或公开面超过 120，都会直接失败。

## 已落地的 trait 试点

`AccountSessions`（`crates/platform/src/facade.rs`）先切 Account 的会话入口：

```rust
pub trait AccountSessions {
    fn lookup_token(&self, token: &str) -> Result<Option<String>>;
    fn revoke_session(&self, token: &str) -> Result<()>;
    fn sessions_of(&self, user_id: &str, current_token: &str) -> Result<Vec<SessionRow>>;
    fn revoke_session_by_sid(&self, user_id: &str, sid: &str) -> Result<bool>;
    fn revoke_other_sessions(&self, user_id: &str, keep_token: &str) -> Result<usize>;
}
```

`Platform` 的同名方法收为 `pub(crate)`，实现只转调，不复制业务逻辑。HTTP/WS/
媒体面/SSE 的身份入口和退出登录都改为 `use wxlike_platform::AccountSessions`，
不再需要认识 `Platform` 其余方法。会话列表/按设备下线/清其它会话也走同一
trait（`SessionRow` 从 crate 根再导出）；公开面因此从 107 降到 102。

## 下一步

- Account 继续把 `create_session` 并入 `AccountSessions`，再处理 TOTP/recovery；
- Realtime 按同样方式先切 `call_state` / `start_call` / `join_call` 的读入口；
- Wallet 保持独立，等 ledger 端口化时一起迁移；
- Social 最大，先拆读路径与写路径，不一次性动 30 个方法。
