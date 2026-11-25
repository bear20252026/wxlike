//! `Platform` 门面试点：先从 Account 的会话能力切出一个窄 trait。
//!
//! 这里不是“把 101 个 pub 方法一次搬进 8 个 trait”，那会把一个可评审的
//! 接口隔离动作变成大规模改签名。试点只做一条真实调用链：
//! `lookup_token` 是 HTTP/WS/媒体面共用的身份入口，`revoke_session` 是退出登录；
//! 两者都归 Account，但不需要调用方认识 `Platform` 的其余 100 多个方法。
//!
//! 约定：
//! - trait 方法必须对应一个真实跨 crate 调用点；没有调用点的“门面”不建。
//! - 实现只转调 `Platform` 的 `pub(crate)` 方法，不复制业务逻辑。
//! - 新方法先加到这里，再把 `Platform` 的同名方法收成 `pub(crate)`；调用方
//!   通过 `use wxlike_platform::AccountSessions` 拿到接口。

use crate::error::Result;
use crate::Platform;

/// Account 门面的会话切片：凭据换身份、退出登录。
pub trait AccountSessions {
    /// 凭据 → 用户 id。`None` 表示凭据无效、过期或已吊销。
    fn lookup_token(&self, token: &str) -> Result<Option<String>>;

    /// 吊销一条会话（按原始 token）。
    fn revoke_session(&self, token: &str) -> Result<()>;

    /// 列出该用户的会话；`current_token` 标出“当前会话”。
    fn sessions_of(&self, user_id: &str, current_token: &str) -> Result<Vec<crate::SessionRow>>;

    /// 按 `sid` 吊销一条会话；`false` = 本来就不存在/已吊销。
    fn revoke_session_by_sid(&self, user_id: &str, sid: &str) -> Result<bool>;

    /// 吊销除当前 token 外的全部会话，返回吊销条数。
    fn revoke_other_sessions(&self, user_id: &str, keep_token: &str) -> Result<usize>;

    /// 签发一条会话（登录/内部签发共用），返回原始 token。
    fn create_session(&self, user_id: &str, ttl_ms: u64) -> Result<String>;

    /// 带设备信息的会话签发：设备目录与「当前设备」标记由这一层落。
    fn create_session_for_device(
        &self,
        user_id: &str,
        ttl_ms: u64,
        device_id: &str,
        device_name: &str,
        ip: &str,
    ) -> Result<String>;
}

impl AccountSessions for Platform {
    fn lookup_token(&self, token: &str) -> Result<Option<String>> {
        Platform::lookup_token(self, token)
    }

    fn revoke_session(&self, token: &str) -> Result<()> {
        Platform::revoke_session(self, token)
    }

    fn sessions_of(&self, user_id: &str, current_token: &str) -> Result<Vec<crate::SessionRow>> {
        Platform::sessions_of(self, user_id, current_token)
    }

    fn revoke_session_by_sid(&self, user_id: &str, sid: &str) -> Result<bool> {
        Platform::revoke_session_by_sid(self, user_id, sid)
    }

    fn revoke_other_sessions(&self, user_id: &str, keep_token: &str) -> Result<usize> {
        Platform::revoke_other_sessions(self, user_id, keep_token)
    }

    fn create_session(&self, user_id: &str, ttl_ms: u64) -> Result<String> {
        Platform::create_session(self, user_id, ttl_ms)
    }

    fn create_session_for_device(
        &self,
        user_id: &str,
        ttl_ms: u64,
        device_id: &str,
        device_name: &str,
        ip: &str,
    ) -> Result<String> {
        Platform::create_session_for_device(self, user_id, ttl_ms, device_id, device_name, ip)
    }
}

/// Governance 门面的管理面鉴权切片：管理令牌是否有效。
pub trait AdminAccess {
    fn admin_ok(&self, presented: Option<&str>) -> bool;
}

impl AdminAccess for Platform {
    fn admin_ok(&self, presented: Option<&str>) -> bool {
        Platform::admin_ok(self, presented)
    }
}

/// Messaging 门面的会话读权限切片：消息落库前的最后一道授权。
pub trait ConversationAccess {
    /// 该用户能否读这个会话（直聊参与者 / 群成员 / 频道可见性）。
    fn may_read_conv(&self, user_id: &str, conv_id: &str) -> bool;

    /// 这个会话是否开启秘密聊天（决定字段脱敏与密钥协商路径）。
    fn is_secret_conv(&self, conv_id: &str) -> Result<bool>;
}

impl ConversationAccess for Platform {
    fn may_read_conv(&self, user_id: &str, conv_id: &str) -> bool {
        Platform::may_read_conv(self, user_id, conv_id)
    }

    fn is_secret_conv(&self, conv_id: &str) -> Result<bool> {
        Platform::is_secret_conv(self, conv_id)
    }
}

/// Wallet 门面：记账、转账、红包与总量核对。
///
/// `fund` / `transfer` / `wallet_balance` 有真实平台外调用（commerce HTTP、
/// ledger 守恒测试）；红包与总量只有验收用例在用，但归同一张账，一并切。
pub trait WalletLedger {
    fn wallet_balance(&self, user_id: &str) -> Result<i64>;
    fn fund(&self, user_id: &str, amount: i64, memo: &str) -> Result<()>;
    fn transfer(&self, from: &str, to: &str, amount: i64, memo: &str) -> Result<String>;
    fn create_redpacket(
        &self,
        from: &str,
        amount: i64,
        count: i64,
        memo: &str,
        conv_id: Option<&str>,
    ) -> Result<String>;
    fn redpacket(&self, id: &str) -> Result<Option<crate::RedpacketRow>>;
    fn claim_redpacket(&self, id: &str, by: &str) -> Result<i64>;
    fn sweep_expired_redpackets(&self) -> Result<usize>;
    fn total_money(&self) -> Result<i64>;
    fn money_supply(&self) -> Result<i64>;
}

impl WalletLedger for Platform {
    fn wallet_balance(&self, user_id: &str) -> Result<i64> {
        Platform::wallet_balance(self, user_id)
    }

    fn fund(&self, user_id: &str, amount: i64, memo: &str) -> Result<()> {
        Platform::fund(self, user_id, amount, memo)
    }

    fn transfer(&self, from: &str, to: &str, amount: i64, memo: &str) -> Result<String> {
        Platform::transfer(self, from, to, amount, memo)
    }

    fn create_redpacket(
        &self,
        from: &str,
        amount: i64,
        count: i64,
        memo: &str,
        conv_id: Option<&str>,
    ) -> Result<String> {
        Platform::create_redpacket(self, from, amount, count, memo, conv_id)
    }

    fn redpacket(&self, id: &str) -> Result<Option<crate::RedpacketRow>> {
        Platform::redpacket(self, id)
    }

    fn claim_redpacket(&self, id: &str, by: &str) -> Result<i64> {
        Platform::claim_redpacket(self, id, by)
    }

    fn sweep_expired_redpackets(&self) -> Result<usize> {
        Platform::sweep_expired_redpackets(self)
    }

    fn total_money(&self) -> Result<i64> {
        Platform::total_money(self)
    }

    fn money_supply(&self) -> Result<i64> {
        Platform::money_supply(self)
    }
}

/// Social 门面的社区/频道切片：建群、入群、频道角色与频道消息。
///
/// 十条方法里有九条只有验收用例在调，`send_channel_message` 是 message 面
/// 真正要走的投递路径；归一张门面，调用方不必认识 `Platform` 的其余面。
pub trait CommunityOps {
    fn channel_send_allowed(&self, channel: &str, user: &str) -> Result<bool>;
    fn effective_channel_role(
        &self,
        channel: &str,
        user: &str,
    ) -> Result<Option<crate::ChannelRole>>;
    fn create_community(&self, owner: &str, name: &str, member_limit: i64) -> Result<String>;
    fn join_community(&self, community: &str, user: &str) -> Result<&'static str>;
    fn create_channel(
        &self,
        community: &str,
        actor: &str,
        name: &str,
        kind: &str,
    ) -> Result<String>;
    fn send_channel_message(
        &self,
        channel: &str,
        actor: &str,
        type_id: &str,
        text: &str,
    ) -> Result<(u64, String)>;
    fn channel_roles(&self, channel: &str, actor: &str) -> Result<Vec<crate::ChannelRoleRow>>;
    fn set_channel_role(
        &self,
        channel: &str,
        actor: &str,
        target: &str,
        role: &str,
    ) -> Result<crate::ChannelRole>;
    fn create_thread(
        &self,
        channel: &str,
        actor: &str,
        root_seq: i64,
        title: &str,
    ) -> Result<String>;
    fn thread_fanout(&self, thread: &str) -> Result<Vec<String>>;
}

impl CommunityOps for Platform {
    fn channel_send_allowed(&self, channel: &str, user: &str) -> Result<bool> {
        Platform::channel_send_allowed(self, channel, user)
    }

    fn effective_channel_role(
        &self,
        channel: &str,
        user: &str,
    ) -> Result<Option<crate::ChannelRole>> {
        Platform::effective_channel_role(self, channel, user)
    }

    fn create_community(&self, owner: &str, name: &str, member_limit: i64) -> Result<String> {
        Platform::create_community(self, owner, name, member_limit)
    }

    fn join_community(&self, community: &str, user: &str) -> Result<&'static str> {
        Platform::join_community(self, community, user)
    }

    fn create_channel(
        &self,
        community: &str,
        actor: &str,
        name: &str,
        kind: &str,
    ) -> Result<String> {
        Platform::create_channel(self, community, actor, name, kind)
    }

    fn send_channel_message(
        &self,
        channel: &str,
        actor: &str,
        type_id: &str,
        text: &str,
    ) -> Result<(u64, String)> {
        Platform::send_channel_message(self, channel, actor, type_id, text)
    }

    fn channel_roles(&self, channel: &str, actor: &str) -> Result<Vec<crate::ChannelRoleRow>> {
        Platform::channel_roles(self, channel, actor)
    }

    fn set_channel_role(
        &self,
        channel: &str,
        actor: &str,
        target: &str,
        role: &str,
    ) -> Result<crate::ChannelRole> {
        Platform::set_channel_role(self, channel, actor, target, role)
    }

    fn create_thread(
        &self,
        channel: &str,
        actor: &str,
        root_seq: i64,
        title: &str,
    ) -> Result<String> {
        Platform::create_thread(self, channel, actor, root_seq, title)
    }

    fn thread_fanout(&self, thread: &str) -> Result<Vec<String>> {
        Platform::thread_fanout(self, thread)
    }
}

/// Social 门面的收藏切片。
pub trait Favorites {
    fn add_favorite(&self, user_id: &str, input: &crate::FavoriteInput) -> Result<crate::Favorite>;
    fn list_favorites(
        &self,
        user_id: &str,
        before_created: Option<i64>,
        limit: usize,
    ) -> Result<Vec<crate::Favorite>>;
    fn favorite(&self, user_id: &str, id: &str) -> Result<crate::Favorite>;
    fn remove_favorite(&self, user_id: &str, id: &str) -> Result<()>;
}

impl Favorites for Platform {
    fn add_favorite(&self, user_id: &str, input: &crate::FavoriteInput) -> Result<crate::Favorite> {
        Platform::add_favorite(self, user_id, input)
    }

    fn list_favorites(
        &self,
        user_id: &str,
        before_created: Option<i64>,
        limit: usize,
    ) -> Result<Vec<crate::Favorite>> {
        Platform::list_favorites(self, user_id, before_created, limit)
    }

    fn favorite(&self, user_id: &str, id: &str) -> Result<crate::Favorite> {
        Platform::favorite(self, user_id, id)
    }

    fn remove_favorite(&self, user_id: &str, id: &str) -> Result<()> {
        Platform::remove_favorite(self, user_id, id)
    }
}

/// Social 门面的好友申请切片：收发申请、接受/拒绝、两端列表。
pub trait FriendRequests {
    fn send_friend_request(
        &self,
        from_uid: &str,
        to_uid: &str,
        message: &str,
    ) -> Result<(String, String)>;
    fn accept_request(&self, me: &str, req_id: &str) -> Result<crate::RequestRow>;
    fn decline_request(&self, me: &str, req_id: &str) -> Result<crate::RequestRow>;
    fn incoming_requests(&self, me: &str) -> Result<Vec<crate::RequestRow>>;
    fn outgoing_requests(&self, me: &str) -> Result<Vec<crate::RequestRow>>;
}

impl FriendRequests for Platform {
    fn send_friend_request(
        &self,
        from_uid: &str,
        to_uid: &str,
        message: &str,
    ) -> Result<(String, String)> {
        Platform::send_friend_request(self, from_uid, to_uid, message)
    }

    fn accept_request(&self, me: &str, req_id: &str) -> Result<crate::RequestRow> {
        Platform::accept_request(self, me, req_id)
    }

    fn decline_request(&self, me: &str, req_id: &str) -> Result<crate::RequestRow> {
        Platform::decline_request(self, me, req_id)
    }

    fn incoming_requests(&self, me: &str) -> Result<Vec<crate::RequestRow>> {
        Platform::incoming_requests(self, me)
    }

    fn outgoing_requests(&self, me: &str) -> Result<Vec<crate::RequestRow>> {
        Platform::outgoing_requests(self, me)
    }
}

/// Governance 门面的端到端加密与权限切片。
pub trait AccountSecurity {
    fn set_secret_chat(
        &self,
        user_id: &str,
        conv_id: &str,
        enabled: bool,
        ttl_ms: u64,
    ) -> Result<crate::SecretState>;
    fn safety_number(&self, a: &str, b: &str) -> Result<String>;
    fn verify_peer(&self, user_id: &str, peer_id: &str, peer_identity: &str) -> Result<()>;
    fn verification_of(&self, user_id: &str, peer_id: &str) -> Result<Option<i64>>;
    fn effective_perms(&self, community: &str, user: &str) -> Result<Vec<&'static str>>;
}

impl AccountSecurity for Platform {
    fn set_secret_chat(
        &self,
        user_id: &str,
        conv_id: &str,
        enabled: bool,
        ttl_ms: u64,
    ) -> Result<crate::SecretState> {
        Platform::set_secret_chat(self, user_id, conv_id, enabled, ttl_ms)
    }

    fn safety_number(&self, a: &str, b: &str) -> Result<String> {
        Platform::safety_number(self, a, b)
    }

    fn verify_peer(&self, user_id: &str, peer_id: &str, peer_identity: &str) -> Result<()> {
        Platform::verify_peer(self, user_id, peer_id, peer_identity)
    }

    fn verification_of(&self, user_id: &str, peer_id: &str) -> Result<Option<i64>> {
        Platform::verification_of(self, user_id, peer_id)
    }

    fn effective_perms(&self, community: &str, user: &str) -> Result<Vec<&'static str>> {
        Platform::effective_perms(self, community, user)
    }
}

/// Governance 门面的审核队列切片。
pub trait ModerationDesk {
    fn set_moderation_mode(&self, mode: wxlike_moderation::Mode);
    fn moderation_queue(&self, limit: usize) -> Result<Vec<crate::QueuedItem>>;
    fn release_moderated(&self, id: &str, actor: &str) -> Result<Option<crate::QueuedItem>>;
}

impl ModerationDesk for Platform {
    fn set_moderation_mode(&self, mode: wxlike_moderation::Mode) {
        Platform::set_moderation_mode(self, mode)
    }

    fn moderation_queue(&self, limit: usize) -> Result<Vec<crate::QueuedItem>> {
        Platform::moderation_queue(self, limit)
    }

    fn release_moderated(&self, id: &str, actor: &str) -> Result<Option<crate::QueuedItem>> {
        Platform::release_moderated(self, id, actor)
    }
}

/// Governance 门面的审计切片：只写、自查两条。
///
/// admin 的运维面是最大的调用方（56 处），但它只需要"记一笔"和"看自己的"，
/// 不需要认识 `Platform` 的其余方法。
pub trait AuditTrail {
    fn audit(&self, actor: &str, action: &str, target: &str, detail: &str);
    fn audit_of(&self, actor: &str, limit: usize) -> Result<Vec<(i64, String, String, String)>>;
}

impl AuditTrail for Platform {
    fn audit(&self, actor: &str, action: &str, target: &str, detail: &str) {
        Platform::audit(self, actor, action, target, detail)
    }

    fn audit_of(&self, actor: &str, limit: usize) -> Result<Vec<(i64, String, String, String)>> {
        Platform::audit_of(self, actor, limit)
    }
}

/// Governance 门面的举报工单切片。
pub trait ReportDesk {
    fn create_report(
        &self,
        reporter: &str,
        target_kind: &str,
        target_id: &str,
        reason: wxlike_report::Reason,
        detail: &str,
    ) -> Result<wxlike_report::Ticket>;
    fn report(&self, id: &str) -> Result<Option<wxlike_report::Ticket>>;
    fn reports_by(&self, reporter: &str, limit: usize) -> Result<Vec<wxlike_report::Ticket>>;
    fn report_queue(&self, state: Option<&str>, limit: usize)
        -> Result<Vec<wxlike_report::Ticket>>;
    fn triage_report(&self, id: &str, actor: &str, note: &str) -> Result<wxlike_report::Ticket>;
    fn act_on_report(
        &self,
        id: &str,
        actor: &str,
        action: wxlike_report::Action,
        note: &str,
    ) -> Result<wxlike_report::Ticket>;
    fn close_report(&self, id: &str, actor: &str, note: &str) -> Result<wxlike_report::Ticket>;
    fn reject_report(&self, id: &str, actor: &str, note: &str) -> Result<wxlike_report::Ticket>;
}

impl ReportDesk for Platform {
    fn create_report(
        &self,
        reporter: &str,
        target_kind: &str,
        target_id: &str,
        reason: wxlike_report::Reason,
        detail: &str,
    ) -> Result<wxlike_report::Ticket> {
        Platform::create_report(self, reporter, target_kind, target_id, reason, detail)
    }

    fn report(&self, id: &str) -> Result<Option<wxlike_report::Ticket>> {
        Platform::report(self, id)
    }

    fn reports_by(&self, reporter: &str, limit: usize) -> Result<Vec<wxlike_report::Ticket>> {
        Platform::reports_by(self, reporter, limit)
    }

    fn report_queue(
        &self,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<wxlike_report::Ticket>> {
        Platform::report_queue(self, state, limit)
    }

    fn triage_report(&self, id: &str, actor: &str, note: &str) -> Result<wxlike_report::Ticket> {
        Platform::triage_report(self, id, actor, note)
    }

    fn act_on_report(
        &self,
        id: &str,
        actor: &str,
        action: wxlike_report::Action,
        note: &str,
    ) -> Result<wxlike_report::Ticket> {
        Platform::act_on_report(self, id, actor, action, note)
    }

    fn close_report(&self, id: &str, actor: &str, note: &str) -> Result<wxlike_report::Ticket> {
        Platform::close_report(self, id, actor, note)
    }

    fn reject_report(&self, id: &str, actor: &str, note: &str) -> Result<wxlike_report::Ticket> {
        Platform::reject_report(self, id, actor, note)
    }
}

/// Social 门面的好友/关系读写切片。
///
/// `may_message` 是 relations 端口（`features/relations`）唯一需要的判定；
/// `friends` / `changes` 是客户端资料页与增量同步的真实入口。
pub trait SocialRelations {
    fn friends(&self, me: &str) -> Result<Vec<crate::FriendRow>>;
    fn changes(&self, me: &str, since: i64, limit: i64) -> Result<Vec<crate::ChangeRow>>;
    fn changes_head(&self, me: &str) -> Result<i64>;
    fn may_message(&self, from: &str, to: &str) -> Result<wxlike_relations::port::MessageGate>;
    fn shared_group_ids(&self, a: &str, b: &str) -> Result<Vec<String>>;
}

impl SocialRelations for Platform {
    fn friends(&self, me: &str) -> Result<Vec<crate::FriendRow>> {
        Platform::friends(self, me)
    }

    fn changes(&self, me: &str, since: i64, limit: i64) -> Result<Vec<crate::ChangeRow>> {
        Platform::changes(self, me, since, limit)
    }

    fn changes_head(&self, me: &str) -> Result<i64> {
        Platform::changes_head(self, me)
    }

    fn may_message(&self, from: &str, to: &str) -> Result<wxlike_relations::port::MessageGate> {
        Platform::may_message(self, from, to)
    }

    fn shared_group_ids(&self, a: &str, b: &str) -> Result<Vec<String>> {
        Platform::shared_group_ids(self, a, b)
    }
}

/// Social 门面的群成员切片：给媒体面做"我属于哪些群"的事实查询。
pub trait GroupMembership {
    fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>>;
}

impl GroupMembership for Platform {
    fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>> {
        Platform::groups_of_user(self, user_id)
    }
}

/// Account 门面的设备目录切片：推送面要知道"这个人有哪些设备"。
pub trait AccountDevices {
    fn devices_of(&self, user_id: &str) -> Result<Vec<crate::DeviceRow>>;
}

impl AccountDevices for Platform {
    fn devices_of(&self, user_id: &str) -> Result<Vec<crate::DeviceRow>> {
        Platform::devices_of(self, user_id)
    }
}

/// Messaging 门面的免打扰切片：hub 决策层查会话级通知规则。
pub trait MessagingPrefs {
    fn notification_rule(&self, user: &str, conv: &str) -> Result<Option<crate::NotificationRule>>;
}

impl MessagingPrefs for Platform {
    fn notification_rule(&self, user: &str, conv: &str) -> Result<Option<crate::NotificationRule>> {
        Platform::notification_rule(self, user, conv)
    }
}

/// Social 门面的关系判定切片：投递策略问"这两个人能不能直发"。
pub trait SocialGraph {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool>;
    fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool>;
    fn is_blocked(&self, user: &str, by: &str) -> Result<bool>;
}

impl SocialGraph for Platform {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool> {
        Platform::are_friends(self, a, b)
    }

    fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool> {
        Platform::stranger_accepted(self, from, to)
    }

    fn is_blocked(&self, user: &str, by: &str) -> Result<bool> {
        Platform::is_blocked(self, user, by)
    }
}

/// Governance 门面的发送闸门切片：秘密会话 TTL 与内容审核裁决。
pub trait GovernanceGate {
    fn secret_state(&self, conv_id: &str) -> Result<crate::SecretState>;
    fn moderate_for_send(
        &self,
        sender: &str,
        conv_id: &str,
        text: &str,
    ) -> Result<crate::SendVerdict>;
}

impl GovernanceGate for Platform {
    fn secret_state(&self, conv_id: &str) -> Result<crate::SecretState> {
        Platform::secret_state(self, conv_id)
    }

    fn moderate_for_send(
        &self,
        sender: &str,
        conv_id: &str,
        text: &str,
    ) -> Result<crate::SendVerdict> {
        Platform::moderate_for_send(self, sender, conv_id, text)
    }
}

/// Realtime 门面的通话/直播控制切片。
///
/// 平台外真正的调用点是 `sfu` 的成员判定与集成测试（测试也是外部 crate）；
/// HTTP 面自己走 `pub(crate)`，不需要这层。trait 存在的意义是让调用方
/// 只说"我要开会/开播"，而不是认识 `Platform` 的其余方法。
pub trait RealtimeCalls {
    fn conv_member_allowed(&self, conv: &str, user: &str) -> Result<bool>;
    fn call_state(&self, conv: &str, user: &str) -> Result<Option<crate::CallView>>;
    fn start_call(&self, conv: &str, user: &str, capacity: usize) -> Result<()>;
    fn join_call(&self, conv: &str, user: &str) -> Result<()>;
    fn leave_call(&self, conv: &str, user: &str) -> Result<()>;
    fn mute_in_call(
        &self,
        conv: &str,
        actor: &str,
        target: &str,
        track: wxlike_rtc::room::Track,
        muted: bool,
    ) -> Result<()>;
    fn mute_all_in_call(
        &self,
        conv: &str,
        actor: &str,
        track: wxlike_rtc::room::Track,
        muted: bool,
    ) -> Result<usize>;
    fn kick_from_call(&self, conv: &str, actor: &str, target: &str) -> Result<()>;
    fn lock_call(&self, conv: &str, actor: &str, locked: bool) -> Result<()>;
    fn record_call(&self, conv: &str, actor: &str, recording: bool) -> Result<()>;
    fn hand_in_call(&self, conv: &str, user: &str, raised: bool) -> Result<()>;
    fn transfer_call(&self, conv: &str, actor: &str, target: &str) -> Result<()>;
    fn call_egress(
        &self,
        conv: &str,
        user: &str,
        estimate_bps: u64,
    ) -> Result<wxlike_rtc::room::EgressPlan>;
    fn post_call_signal(
        &self,
        conv: &str,
        actor: &str,
        signal: wxlike_rtc::signal::Signal,
    ) -> Result<usize>;
    fn take_call_signals(&self, conv: &str, user: &str) -> Result<Vec<wxlike_rtc::signal::Signal>>;
    fn ingest_token(&self, stream_key: &str, ttl_ms: u64) -> String;
    fn verify_ingest(&self, stream_key: &str, token: &str, now_ms: u64) -> Result<()>;
    fn start_live(&self, channel: &str, actor: &str, recording: bool)
        -> Result<crate::LiveSession>;
    fn stop_live(&self, channel: &str, actor: &str) -> Result<()>;
    fn live_state(&self, channel: &str, user: &str) -> Result<crate::LiveState>;
    fn join_live(&self, channel: &str, user: &str) -> Result<i64>;
    fn leave_live(&self, channel: &str, user: &str) -> Result<i64>;
}

impl RealtimeCalls for Platform {
    fn conv_member_allowed(&self, conv: &str, user: &str) -> Result<bool> {
        Platform::conv_member_allowed(self, conv, user)
    }

    fn call_state(&self, conv: &str, user: &str) -> Result<Option<crate::CallView>> {
        Platform::call_state(self, conv, user)
    }

    fn start_call(&self, conv: &str, user: &str, capacity: usize) -> Result<()> {
        Platform::start_call(self, conv, user, capacity)
    }

    fn join_call(&self, conv: &str, user: &str) -> Result<()> {
        Platform::join_call(self, conv, user)
    }

    fn leave_call(&self, conv: &str, user: &str) -> Result<()> {
        Platform::leave_call(self, conv, user)
    }

    fn mute_in_call(
        &self,
        conv: &str,
        actor: &str,
        target: &str,
        track: wxlike_rtc::room::Track,
        muted: bool,
    ) -> Result<()> {
        Platform::mute_in_call(self, conv, actor, target, track, muted)
    }

    fn mute_all_in_call(
        &self,
        conv: &str,
        actor: &str,
        track: wxlike_rtc::room::Track,
        muted: bool,
    ) -> Result<usize> {
        Platform::mute_all_in_call(self, conv, actor, track, muted)
    }

    fn kick_from_call(&self, conv: &str, actor: &str, target: &str) -> Result<()> {
        Platform::kick_from_call(self, conv, actor, target)
    }

    fn lock_call(&self, conv: &str, actor: &str, locked: bool) -> Result<()> {
        Platform::lock_call(self, conv, actor, locked)
    }

    fn record_call(&self, conv: &str, actor: &str, recording: bool) -> Result<()> {
        Platform::record_call(self, conv, actor, recording)
    }

    fn hand_in_call(&self, conv: &str, user: &str, raised: bool) -> Result<()> {
        Platform::hand_in_call(self, conv, user, raised)
    }

    fn transfer_call(&self, conv: &str, actor: &str, target: &str) -> Result<()> {
        Platform::transfer_call(self, conv, actor, target)
    }

    fn call_egress(
        &self,
        conv: &str,
        user: &str,
        estimate_bps: u64,
    ) -> Result<wxlike_rtc::room::EgressPlan> {
        Platform::call_egress(self, conv, user, estimate_bps)
    }

    fn post_call_signal(
        &self,
        conv: &str,
        actor: &str,
        signal: wxlike_rtc::signal::Signal,
    ) -> Result<usize> {
        Platform::post_call_signal(self, conv, actor, signal)
    }

    fn take_call_signals(&self, conv: &str, user: &str) -> Result<Vec<wxlike_rtc::signal::Signal>> {
        Platform::take_call_signals(self, conv, user)
    }

    fn ingest_token(&self, stream_key: &str, ttl_ms: u64) -> String {
        Platform::ingest_token(self, stream_key, ttl_ms)
    }

    fn verify_ingest(&self, stream_key: &str, token: &str, now_ms: u64) -> Result<()> {
        Platform::verify_ingest(self, stream_key, token, now_ms)
    }

    fn start_live(
        &self,
        channel: &str,
        actor: &str,
        recording: bool,
    ) -> Result<crate::LiveSession> {
        Platform::start_live(self, channel, actor, recording)
    }

    fn stop_live(&self, channel: &str, actor: &str) -> Result<()> {
        Platform::stop_live(self, channel, actor)
    }

    fn live_state(&self, channel: &str, user: &str) -> Result<crate::LiveState> {
        Platform::live_state(self, channel, user)
    }

    fn join_live(&self, channel: &str, user: &str) -> Result<i64> {
        Platform::join_live(self, channel, user)
    }

    fn leave_live(&self, channel: &str, user: &str) -> Result<i64> {
        Platform::leave_live(self, channel, user)
    }
}

/// Social 门面的群禁言切片：投递策略问"这个人在群里被禁言了吗"。
pub trait GroupModeration {
    fn is_muted(&self, group: &str, user: &str) -> Result<bool>;
}

impl GroupModeration for Platform {
    fn is_muted(&self, group: &str, user: &str) -> Result<bool> {
        Platform::is_muted(self, group, user)
    }
}
