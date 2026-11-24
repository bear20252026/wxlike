# Platform 门面归属表

`Platform` 的公开方法已经压到 90 个，但“少”不等于“有结构”。这份表把每个
公开方法归到一个门面，作为后续拆 trait / 收敛组合根的边界。

## 归属

| 门面 | 模块 | 当前 pub 方法 |
|---|---|---|
| Account | `sessions` / `security` / `recovery` / `devices` | 11 |
| Messaging | `read` / `notification` | 4 |
| Media | `video_refs` | 1 |
| Wallet | `wallet` | 6 |
| Governance | `report` / `audit` / `secret` / `perms` / `trust` | 18 |
| Realtime | `call` / `live` | 24 |
| Social | `communities` / `friends` / `relations` / `favorites` / `status` / `groups_admin` | 26 |
| Health | 组合根自身（readiness） | 0 |

合计 **90**。

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

## 访问判定 trait

server 在鉴权/落库前要问平台四件事，这些是真跨 crate 调用，因此各收一个窄 trait：

```rust
pub trait AdminAccess { fn admin_ok(&self, presented: Option<&str>) -> bool; }
pub trait ConversationAccess {
    fn may_read_conv(&self, user_id: &str, conv_id: &str) -> bool;
    fn is_secret_conv(&self, conv_id: &str) -> Result<bool>;
}
pub trait GroupMembership { fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>>; }
```

`AdminAccess` 归 Governance，`ConversationAccess` 归 Messaging，`GroupMembership`
归 Social。`server/src/lib.rs`、`commerce_mount.rs`、`media_mount.rs` 改为
`use wxlike_platform::{AdminAccess, ConversationAccess, GroupMembership}`；
`Platform` 的同名 inherent 方法收成 `pub(crate)`。公开面 102→**101**
（审计脚本对本批只计到 `is_secret_conv` 一处，其余三个同名方法在它眼里
本来就没进计数，见 `platform-surface.md` 的口径说明）。

## 投递策略与设备目录切片

`server` 的 `PlatformPolicy` / `PlatformDevices` / `PlatformRules` 三个适配器原先直接
调 `Platform` 的七个判定方法。它们各自归口不同门面，于是按语义拆成五个窄 trait：

```rust
pub trait SocialGraph {            // Social
    fn are_friends(&self, a: &str, b: &str) -> Result<bool>;
    fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool>;
    fn is_blocked(&self, user: &str, by: &str) -> Result<bool>;
}
pub trait GroupModeration { fn is_muted(&self, group: &str, user: &str) -> Result<bool>; }
pub trait GovernanceGate {         // Governance
    fn secret_state(&self, conv_id: &str) -> Result<SecretState>;
    fn moderate_for_send(&self, sender: &str, conv_id: &str, text: &str) -> Result<SendVerdict>;
}
pub trait AccountDevices { fn devices_of(&self, user_id: &str) -> Result<Vec<DeviceRow>>; }
pub trait MessagingPrefs {
    fn notification_rule(&self, user: &str, conv: &str) -> Result<Option<NotificationRule>>;
}
```

`Platform` 的同名 inherent 方法全部收为 `pub(crate)`，实现只转调；`SecretState` /
`SendVerdict` / `NotificationRule` 从 crate 根再导出，调用方不需要认识 domain 模块。
`server/src/lib.rs` 只多 `use` 五个 trait，平台集成测试按用到的能力各加一行
`use wxlike_platform::…`。公开面 101→**90**（Account 12→11、Messaging 5→4、
Governance 20→18、Social 30→26；Wallet 的 9 在审计口径里一直是 6）。

## 下一步

- Account 继续把 `create_session` 并入 `AccountSessions`，再处理 TOTP/recovery；
- Realtime 的 24 个方法目前没有平台外调用点（只有 platform 自己的 HTTP 面与测试），
  按“无调用点不建 trait”的约定暂不切；
- Wallet 保持独立，等 ledger 端口化时一起迁移；
- Social 剩下的写路径（`communities` / `friends`）仍是大头，先拆读路径再动写路径。
