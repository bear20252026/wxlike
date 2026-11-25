# Platform 门面归属表

`Platform` 的**业务方法公开面已经清零**：276 个方法里 0 个 `pub`，全部经
`facade.rs` 的窄 trait 暴露。这份表记录每个门面的归属，防止新方法又长回
`Platform` 身上——`pub` 只能是构造器，不能是业务动作。

## 归属

| 门面 | 模块 | 当前 pub 方法 |
|---|---|---|
| Account | `sessions` / `security` / `recovery` / `devices` | 0 |
| Messaging | `read` / `notification` | 0 |
| Media | `video_refs` | 0 |
| Wallet | `wallet` | 0 |
| Governance | `report` / `audit` / `secret` / `perms` / `trust` | 0 |
| Realtime | `call` / `live` | 0 |
| Social | `communities` / `friends` / `relations` / `favorites` / `status` / `groups_admin` | 0 |
| Health | 组合根自身（readiness） | 0 |

合计 **0**。

## 规则

1. 新业务动作必须归入上表某个门面的 trait；`Platform` 上不再新增 `pub fn`，归不进去说明边界没想清楚。
2. 门面之间不互相调用；跨门面协作走 domain 层或显式端口，不经过 `Platform` 的
   另一个公开方法。
3. 门面内部实现优先 `pub(crate)`；只有真实跨 crate 入口才 `pub`。
4. 后续拆 trait 时按这张表逐个门面迁移，不按文件零散改签名。

## 门禁

`ci.yml` 的 `fmt` job 执行：

```bash
python3 scripts/platform_surface_audit.py --max-pub 0 --check-facades
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

## Realtime 切片

`call` / `live` 的 22 个控制方法只有两个平台外调用点：`sfu` 的成员判定与集成测试
（`platform/tests/call.rs`、`call_media.rs`、`p5_live.rs`，它们本身也是外部 crate）。
按门面整体切成一个 trait：

```rust
pub trait RealtimeCalls {
    fn conv_member_allowed(&self, conv: &str, user: &str) -> Result<bool>;
    fn call_state(&self, conv: &str, user: &str) -> Result<Option<CallView>>;
    fn start_call(&self, conv: &str, user: &str, capacity: usize) -> Result<()>;
    // join/leave/mute/mute_all/kick/lock/record/hand/transfer/egress
    fn post_call_signal(&self, conv: &str, actor: &str, signal: Signal) -> Result<usize>;
    fn take_call_signals(&self, conv: &str, user: &str) -> Result<Vec<Signal>>;
    // live: ingest_token / verify_ingest / start / stop / state / join / leave
}
```

`Platform` 的同名方法收为 `pub(crate)`；`CallView` / `LiveSession` / `LiveState` 从
crate 根再导出。Realtime 的公开面 24→**2**（`calls()` 与 `CallRegistry` 自身仍公开），
总公开面 90→**68**。

## Governance 的审计与工单切片

admin 的运维面是这两块的唯一真实调用方：审计 56 处、工单 8 个动作，全部走
HTTP 处理器或验收测试。切成两个 trait，类型从 crate 根再导出
（`Ticket` 直接来自 `wxlike_report`）：

```rust
pub trait AuditTrail {              // 只写、自查
    fn audit(&self, actor: &str, action: &str, target: &str, detail: &str);
    fn audit_of(&self, actor: &str, limit: usize) -> Result<Vec<(i64, String, String, String)>>;
}

pub trait ReportDesk {              // 举报工单全生命周期
    fn create_report(/* reporter, target_kind, target_id, Reason, detail */) -> Result<Ticket>;
    fn report(&self, id: &str) -> Result<Option<Ticket>>;
    fn reports_by(&self, reporter: &str, limit: usize) -> Result<Vec<Ticket>>;
    fn report_queue(&self, state: Option<&str>, limit: usize) -> Result<Vec<Ticket>>;
    fn triage_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket>;
    fn act_on_report(&self, id: &str, actor: &str, action: Action, note: &str) -> Result<Ticket>;
    fn close_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket>;
    fn reject_report(&self, id: &str, actor: &str, note: &str) -> Result<Ticket>;
}
```

Governance 公开面 18→**8**，总公开面 68→**62**。

## Social 的好友/关系切片

`may_message` 是 relations 端口的唯一判定入口，`friends` / `changes` /
`changes_head` 是客户端资料页与增量同步的真实入口；`shared_group_ids` 只有关系
验收用例在用。五条合成 `SocialRelations`，`FriendRow` / `ChangeRow` 从 crate 根
再导出。

```rust
pub trait SocialRelations {
    fn friends(&self, me: &str) -> Result<Vec<FriendRow>>;
    fn changes(&self, me: &str, since: i64, limit: i64) -> Result<Vec<ChangeRow>>;
    fn changes_head(&self, me: &str) -> Result<i64>;
    fn may_message(&self, from: &str, to: &str) -> Result<MessageGate>;
    fn shared_group_ids(&self, a: &str, b: &str) -> Result<Vec<String>>;
}
```

Social 公开面 21（裁掉 5 条），总公开面 62→**53**。

## Wallet 切片

`fund` / `transfer` / `wallet_balance` 有真实平台外调用（commerce HTTP、ledger
守恒测试、server），红包与总量只有验收用例在用；九条同属一张账，整体切成
`WalletLedger`，`RedpacketRow` 从 crate 根再导出。Wallet 公开面 6→**0**，
总公开面 53→**47**。

## Social 的社区、收藏、好友申请切片

Communities 的十条方法（建群/入群/建频道/频道角色/频道消息/楼中楼）里，九条
只有验收用例在调，`send_channel_message` 是 message 面真正走的投递路径；
Favorites 四条、FriendRequests 五条也分别只有各自的验收面在调。三组各切一个
trait，`ChannelRole` / `ChannelRoleRow` / `RequestRow` 从 crate 根再导出。

```rust
pub trait CommunityOps { /* channel_send_allowed / effective_channel_role /
    create_community / join_community / create_channel / send_channel_message /
    channel_roles / set_channel_role / create_thread / thread_fanout */ }
pub trait Favorites { /* add_favorite / list_favorites / favorite / remove_favorite */ }
pub trait FriendRequests { /* send_friend_request / accept_request / decline_request /
    incoming_requests / outgoing_requests */ }
```

Social 公开面 21→**2**（只剩 `relations` 的陌生人请求状态与 `groups_admin` 的
成员禁言仍由 server 直接调）。

## Governance 的加密、权限与审核切片

`set_secret_chat` / `safety_number` / `verify_peer` / `verification_of` 与
`effective_perms` 合成 `AccountSecurity`；`trust` 的三条（模式、队列、放行）
合成 `ModerationDesk`。`QueuedItem` 从 crate 根再导出。Governance 公开面 8→**0**。

## Account 会话签发并入

`create_session` / `create_session_for_device` 原本是 inherent pub（24 + 2 处
调用，几乎全是集成测试）；并入 `AccountSessions` 后 Account 公开面 11→**9**，
总公开面 47→**18**。

## 收尾：最后 18 条清零

- Account 9 条拆成三块：TOTP 管理（`totp_enabled` / `totp_confirm` /
  `totp_disable`）并入 `AccountSecurity`；验证码与恢复码 5 条新建
  `AccountRecovery`；`register_device` 并入 `AccountDevices`。
- Messaging 4 条（`conv_unread` / `conv_sync` / `mark_conv_read` /
  `owner_kind_of`）新建 `MailboxRead`。
- Media 的 `video_ref` 新建 `MediaOps`；Realtime 的 `calls` 新建
  `CallInspector`、`send_danmaku` 新建 `LiveOps`（trait 内 `async fn`，只服务
  具体 `Platform`，不装箱）。
- Social 的 `set_member_mute` 并入 `GroupModeration`，`sweep_expired_statuses`
  新建 `StatusOps`。

`Platform` 的 pub 业务方法从 18 降到 **0**；门面 trait 从 19 个增到 25 个（以 `pub trait` 计数为准）。

## 下一步

- 业务面已经没有 `Platform::pub fn`；新增能力先加门面 trait，再实现
  `pub(crate)` 方法——`--max-pub 0` 的 CI 门禁会拒绝任何回潮。
- Wallet 后续 ledger 端口化只需替换 trait 实现，不动调用方。
- 门面 trait 已经稳定，可开始把组合根装配（`PlatformState`）按门面注入，
  让 handler 只持有它需要的那张 trait，而不是整个 `Platform`。
