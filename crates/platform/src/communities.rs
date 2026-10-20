//! P3 支柱 C：社区 / 频道 / 话题（合同 §3.4，ADR-16 三支柱之三）。
//!
//! Schema 形状借用 **TangSengDaoDaoServer**（Apache-2.0，`refs/TangSengDaoDaoServer`
//! `modules/group/sql/`）的群/成员表设计：角色编号 0=成员 1=群主 2=管理员、
//! `member_limit` 上限、`tenant_id` 预留列；频道四类型（文字/公告/语音/直播）
//! 按 Mattermost 的 channel-purpose 模型（MIT，`refs` 未含该仓，仅对照其文档
//! 模型）；话题（Thread）按 Zulip 的 topic 语义——**回复聚合在话题内，不占
//! 频道主时间线**（不变式 3 的直接推论）。
//!
//! 频道时间线**不在此存储**：直接复用 P3-1 读扩散信箱
//! （`wxlike_message::MessageStore`，owner=Channel）——三支柱共用一套信箱
//! 基础设施（不变式 1），本模块只管成员/权限/形态。
//!
//! Zone C（C1–C4）在此之上补三样：
//! * **频道级角色**（`channel_role` 表，owner/moderator/member/guest）——
//!   模型参照 Rocket.Chat 的 channel roles（MIT，未镜像，按其分层与命名重写），
//!   判定函数形状照 [`crate::perms`]，不另起一套；
//! * **按消息类型的发送闸门**（文字 / 附件走不同权限点）；
//! * **话题订阅的静音位 + 扇出名单**（[`Platform::thread_fanout`]）。
//!
//! 两层的分工要说死，否则加一个权限要改两处：
//! **社区角色**管"能不能建频道、踢人、提管理员"；**频道角色**管"在这个频道
//! 里能说什么、能不能撤别人的消息"。频道没有专属角色行时按 owner→owner、
//! admin→moderator、member→member 继承社区角色。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// TangSeng 借用的角色编号（0 成员 / 1 群主 / 2 管理员）。
pub const ROLE_MEMBER: i64 = 0;
pub const ROLE_OWNER: i64 = 1;
pub const ROLE_ADMIN: i64 = 2;

/// 频道四类型（合同 §3.4）。
pub const CHANNEL_KINDS: [&str; 4] = ["text", "announce", "voice", "live"];

/// 带附件的消息类型：走 `PostMedia` 闸门（C2 的"每条消息类型的发送权限"）。
pub const MEDIA_TYPES: [&str; 5] = [
    "image",
    wxlike_core::msgmeta::rich_kind::FILE,
    wxlike_core::msgmeta::rich_kind::STICKER,
    wxlike_core::msgmeta::rich_kind::VOICE,
    wxlike_core::msgmeta::rich_kind::VIDEO,
];

/// 客户端能在频道里主动发的类型 = `text` + [`MEDIA_TYPES`]。
///
/// 白名单而不是黑名单：`system` / `msg.forward` / `wallet.redpacket` 这些卡片
/// 由服务端逻辑落库，放开端点等于让人**伪造成系统口吻**发消息。
pub const CLIENT_SENDABLE: [&str; 6] = [
    "text",
    "image",
    wxlike_core::msgmeta::rich_kind::FILE,
    wxlike_core::msgmeta::rich_kind::STICKER,
    wxlike_core::msgmeta::rich_kind::VOICE,
    wxlike_core::msgmeta::rich_kind::VIDEO,
];

/// 发这条消息类型需要哪个权限点；注册表里没有的类型返回 `None`。
///
/// 判定顺序是"先问类型、再问频道形态"：公告频道额外要 `PostInAnnounce`，
/// 但类型闸门不会因此放松——公告频道里的图片仍然要同时持有两个点。
pub fn perm_for_type(type_id: &str) -> Option<crate::perms::Perm> {
    if MEDIA_TYPES.contains(&type_id) {
        return Some(crate::perms::Perm::PostMedia);
    }
    match wxlike_message::channel_class_of(type_id) {
        Some(wxlike_message::ChannelClass::Normal) => Some(crate::perms::Perm::PostMessage),
        _ => None,
    }
}

/// 频道级角色（Zone C · C2）。名字与 Rocket.Chat 的频道角色对齐：
/// `owner` / `moderator` / `member` / `guest`。
///
/// 与社区级三档数字角色（[`ROLE_OWNER`] / [`ROLE_ADMIN`] / [`ROLE_MEMBER`]）的
/// 分工：**社区角色决定"能不能建/删频道、踢人、提管理员"，频道角色决定
/// "在这个频道里能说什么、能不能撤别人的消息"**。频道没有专属行时继承社区角色
/// （owner→owner、admin→moderator、member→member），有行则以行内为准——
/// 这样"把某人降成 guest 只准打字不准传图"不必动他的社区身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelRole {
    Owner,
    Moderator,
    Member,
    Guest,
}

impl ChannelRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelRole::Owner => "owner",
            ChannelRole::Moderator => "moderator",
            ChannelRole::Member => "member",
            ChannelRole::Guest => "guest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "owner" => ChannelRole::Owner,
            "moderator" => ChannelRole::Moderator,
            "member" => ChannelRole::Member,
            "guest" => ChannelRole::Guest,
            _ => return None,
        })
    }

    /// 社区角色 → 频道角色的继承表。
    fn from_community(role: i64) -> Self {
        match role {
            ROLE_OWNER => ChannelRole::Owner,
            ROLE_ADMIN => ChannelRole::Moderator,
            _ => ChannelRole::Member,
        }
    }
}

/// 频道角色 → 权限集（纯函数，好测）。形状照 [`crate::perms::role_perms`]。
///
/// * `owner`：这个频道里的一切，含提授频道角色与撤他人消息；
/// * `moderator`：能撤消息、能置顶、能在公告频道发，**不能授予角色**
///   （提权只归 owner，与社区层同一条规矩：被盗号的管理员不该自我复制）；
/// * `member`：打字 + 附件 + 开话题；
/// * `guest`：只能打字、开话题——**附件不行**（C2 的验收项）。
pub fn channel_role_perms(role: ChannelRole) -> &'static [crate::perms::Perm] {
    use crate::perms::Perm;
    const GUEST: &[Perm] = &[Perm::PostMessage, Perm::CreateThread];
    const MEMBER: &[Perm] = &[Perm::PostMessage, Perm::PostMedia, Perm::CreateThread];
    const MODERATOR: &[Perm] = &[
        Perm::PostMessage,
        Perm::PostMedia,
        Perm::PostInAnnounce,
        Perm::CreateThread,
        Perm::PinMessage,
        Perm::DeleteAnyMessage,
    ];
    const OWNER: &[Perm] = &[
        Perm::PostMessage,
        Perm::PostMedia,
        Perm::PostInAnnounce,
        Perm::CreateThread,
        Perm::PinMessage,
        Perm::DeleteAnyMessage,
        Perm::ManageChannel,
        Perm::ManageMembers,
        Perm::ManageRoles,
    ];
    match role {
        ChannelRole::Owner => OWNER,
        ChannelRole::Moderator => MODERATOR,
        ChannelRole::Member => MEMBER,
        ChannelRole::Guest => GUEST,
    }
}

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/communities", post(create_community))
        .route("/api/communities/mine", get(my_communities))
        .route("/api/communities/{id}/join", post(join_community))
        .route("/api/communities/{id}/members", get(community_members))
        .route("/api/communities/{id}/channels", post(create_channel))
        .route("/api/communities/{id}/channels", get(list_channels))
        .route("/api/community/{id}/threads", get(community_threads))
        .route(
            "/api/community/{id}/threads",
            post(create_thread_in_community),
        )
        .route("/api/channels/{id}/messages", post(send_channel_message))
        .route("/api/channels/{id}/messages", get(fetch_channel_messages))
        .route(
            "/api/channels/{id}/messages/recall",
            post(recall_channel_message),
        )
        .route("/api/channels/{id}/threads", post(create_thread))
        .route("/api/channels/{id}/threads", get(list_threads))
        .route("/api/channel/{id}/roles", get(list_channel_roles))
        .route("/api/channel/{id}/roles", post(set_channel_role))
        .route("/api/channel/{id}/roles/{user}", delete(reset_channel_role))
        .route("/api/threads/{id}/messages", post(send_thread_reply))
        .route("/api/threads/{id}/messages", get(fetch_thread_messages))
        .route("/api/threads/{id}/subscribe", post(subscribe_thread))
        .route("/api/threads/{id}/subscribe", put(set_thread_subscription))
        .route("/api/threads/{id}/subscribe", delete(unsubscribe_thread))
        .route("/api/threads/{id}/subscribers", get(thread_subscribers))
}

// ---------------------------------------------------------------------------
// 数据形状
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CommunityRow {
    pub id: String,
    pub name: String,
    pub owner_uid: String,
    pub member_limit: i64,
    pub announcement: String,
    pub tenant_id: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChannelRow {
    pub id: String,
    pub community_id: String,
    pub name: String,
    pub kind: String,
    pub tenant_id: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ThreadRow {
    pub id: String,
    pub channel_id: String,
    pub root_seq: i64,
    pub title: String,
    pub created_by: String,
    pub created_at: i64,
}

/// 频道角色名单的一行（`role` 是有效角色，`inherited` 说明它来自社区角色）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ChannelRoleRow {
    pub user_id: String,
    pub role: String,
    pub inherited: bool,
    pub granted_by: String,
}

/// 社区级话题清单里的一行：话题本体 + 它所在的频道（跨频道聚合时必需）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CommunityThreadRow {
    pub thread_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub root_seq: i64,
    pub title: String,
    pub created_by: String,
    pub created_at: i64,
    pub replies: i64,
    pub subscribed: bool,
}

/// 话题订阅关系（C3）：`muted` = 收得到历史但不再收推送。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ThreadSubscription {
    pub user_id: String,
    pub muted: bool,
}

fn valid_kind(kind: &str) -> bool {
    CHANNEL_KINDS.contains(&kind)
}

impl Platform {
    // ---- 成员/角色（TangSeng 形状） ----

    fn community_role(&self, community: &str, user: &str) -> Result<Option<i64>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT role FROM community_members WHERE community_id = ?1 AND user_id = ?2",
            params![community, user],
            |r| r.get::<_, i64>(0),
        )
        .optional()?)
    }

    pub(crate) fn require_member(&self, community: &str, user: &str) -> Result<i64> {
        self.community_role(community, user)?
            .ok_or(PlatformError::Forbidden)
    }

    pub(crate) fn require_manager(&self, community: &str, user: &str) -> Result<()> {
        match self.require_member(community, user)? {
            r if r == ROLE_OWNER || r == ROLE_ADMIN => Ok(()),
            _ => Err(PlatformError::Forbidden),
        }
    }

    pub(crate) fn channel_community(&self, channel: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT community_id FROM channels WHERE id = ?1",
            params![channel],
            |r| r.get(0),
        )
        .optional()?)
    }

    /// 频道发送裁决：announce 型仅管理员/群主（Mattermost 广播频道语义）；
    /// 其余三型成员可发。直播/语音的时间线在此为过渡承载，P5 媒体面接管。
    /// 频道类型（`text|announce|voice|live`）；不存在返回 `None`。
    pub(crate) fn channel_kind(&self, channel: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT kind FROM channels WHERE id = ?1",
            params![channel],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    }

    pub fn channel_send_allowed(&self, channel: &str, user: &str) -> Result<bool> {
        let perm = if self.channel_kind(channel)?.as_deref() == Some("announce") {
            crate::perms::Perm::PostInAnnounce
        } else {
            crate::perms::Perm::PostMessage
        };
        self.can_in_channel(channel, user, perm)
    }

    // ---- 频道角色（C2）----

    /// 频道专属的那一行；没有行 = 继承社区角色。
    fn channel_role_row(&self, channel: &str, user: &str) -> Result<Option<ChannelRole>> {
        let c = self.db().lock();
        let stored: Option<String> = c
            .query_row(
                "SELECT role FROM channel_role WHERE channel_id = ?1 AND user_id = ?2",
                params![channel, user],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored.and_then(|s| ChannelRole::parse(&s)))
    }

    /// 有效频道角色：专属行优先，其次继承社区角色，都不是成员 = `None`。
    pub fn effective_channel_role(&self, channel: &str, user: &str) -> Result<Option<ChannelRole>> {
        if let Some(role) = self.channel_role_row(channel, user)? {
            return Ok(Some(role));
        }
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        Ok(self
            .community_role(&community, user)?
            .map(ChannelRole::from_community))
    }

    /// 频道内的权限点判定（与 [`Platform::can`] 同形状：没权限返回 false，
    /// 不返回错误——UI 要能把按钮显示成灰的）。频道不存在才报错。
    pub fn can_in_channel(
        &self,
        channel: &str,
        user: &str,
        perm: crate::perms::Perm,
    ) -> Result<bool> {
        Ok(match self.effective_channel_role(channel, user)? {
            Some(role) => channel_role_perms(role).contains(&perm),
            None => false,
        })
    }

    /// 频道内的硬闸门：非成员与缺权限点都是 403。
    fn require_in_channel(
        &self,
        channel: &str,
        user: &str,
        perm: crate::perms::Perm,
    ) -> Result<ChannelRole> {
        let role = self
            .effective_channel_role(channel, user)?
            .ok_or(PlatformError::Forbidden)?;
        if channel_role_perms(role).contains(&perm) {
            Ok(role)
        } else {
            Err(PlatformError::Forbidden)
        }
    }

    /// 发消息要过的三道闸：客户端可发类型 → 类型权限点（文字 / 附件）→ 频道形态。
    fn require_send_allowed(&self, channel: &str, user: &str, type_id: &str) -> Result<()> {
        if !CLIENT_SENDABLE.contains(&type_id) {
            return Err(PlatformError::BadInput(
                "kind must be text|image|msg.file|msg.sticker|msg.voice|msg.video",
            ));
        }
        let by_type = perm_for_type(type_id).ok_or(PlatformError::BadInput(
            "message type not allowed in a channel",
        ))?;
        self.require_in_channel(channel, user, by_type)?;
        if self.channel_kind(channel)?.as_deref() == Some("announce") {
            self.require_in_channel(channel, user, crate::perms::Perm::PostInAnnounce)?;
        }
        Ok(())
    }

    // ---- 社区 ----

    pub fn create_community(&self, owner: &str, name: &str, member_limit: i64) -> Result<String> {
        let name = validate::name(name)?;
        let owner = validate::id(owner)?;
        let member_limit = member_limit.clamp(2, 100_000);
        let id = crate::auth::random_hex(8);
        self.db().lock().execute_batch(&format!(
            "INSERT INTO communities(id, name, owner_uid, member_limit, tenant_id, created_at)
             VALUES('{id}', '{}', '{owner}', {member_limit}, 'default', {});",
            name.replace('\'', "''"),
            wxlike_core::types::now_ms() as i64
        ))?;
        self.db().lock().execute(
            "INSERT INTO community_members(community_id, user_id, role, joined_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![id, owner, ROLE_OWNER, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(id)
    }

    pub fn join_community(&self, community: &str, user: &str) -> Result<&'static str> {
        let community = validate::id(community)?;
        let user = validate::id(user)?;
        if self.community_role(community, user)?.is_some() {
            return Ok("already_member");
        }
        // 锁必须在 sync 之前放掉：sync 自己还要开一次锁，std Mutex 不可重入。
        {
            let c = self.db().lock();
            let (limit, current): (i64, i64) = c.query_row(
                "SELECT member_limit,
                        (SELECT COUNT(*) FROM community_members WHERE community_id = ?1)
                 FROM communities WHERE id = ?1",
                params![community],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if current >= limit {
                return Err(PlatformError::BadInput("community member limit reached"));
            }
            c.execute(
                "INSERT INTO community_members(community_id, user_id, role, joined_at)
                 VALUES(?1, ?2, ?3, ?4)",
                params![
                    community,
                    user,
                    ROLE_MEMBER,
                    wxlike_core::types::now_ms() as i64
                ],
            )?;
        }
        self.sync_community_channels(community)?;
        Ok("joined")
    }

    pub fn my_communities(&self, user: &str) -> Result<Vec<(CommunityRow, i64)>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT c.id, c.name, c.owner_uid, c.member_limit, c.announcement, c.tenant_id,
                    c.created_at, m.role
             FROM communities c JOIN community_members m
               ON m.community_id = c.id AND m.user_id = ?1
             ORDER BY c.created_at",
        )?;
        let rows = stmt.query_map(params![user], |r| {
            Ok((
                CommunityRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    owner_uid: r.get(2)?,
                    member_limit: r.get(3)?,
                    announcement: r.get(4)?,
                    tenant_id: r.get(5)?,
                    created_at: r.get(6)?,
                },
                r.get(7)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn community_members(&self, community: &str, user: &str) -> Result<Vec<(String, i64)>> {
        self.require_member(community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT user_id, role FROM community_members WHERE community_id = ?1
             ORDER BY role DESC, joined_at",
        )?;
        let rows = stmt.query_map(params![community], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- 频道 ----

    pub fn create_channel(
        &self,
        community: &str,
        actor: &str,
        name: &str,
        kind: &str,
    ) -> Result<String> {
        let community = validate::id(community)?;
        self.require_manager(community, actor)?;
        let name = validate::name(name)?;
        if !valid_kind(kind) {
            return Err(PlatformError::BadInput(
                "kind must be text|announce|voice|live",
            ));
        }
        let id = crate::auth::random_hex(8);
        self.db().lock().execute(
            "INSERT INTO channels(id, community_id, name, kind, tenant_id, created_at)
             VALUES(?1, ?2, ?3, ?4, 'default', ?5)",
            params![
                id,
                community,
                name,
                kind,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        // 信箱按属主组织，HTTP 拿到的是 conv_id，所以要落一张映射
        self.set_owner_kind(&id, wxlike_message::OwnerType::Channel)?;
        // 频道的扇出名单从社区成员来（C3）——不落这一步，频道时间线只有作者
        // 自己看得见未读，别人永远收不到。
        self.sync_channel_members(&id)?;
        Ok(id)
    }

    pub fn list_channels(&self, community: &str, user: &str) -> Result<Vec<ChannelRow>> {
        self.require_member(community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, community_id, name, kind, tenant_id, created_at
             FROM channels WHERE community_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map(params![community], row_channel)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 频道发消息：走读扩散信箱（不变式 1/3），按消息类型与频道形态双重闸门。
    /// 返回 `(信箱 seq, msg_id)`——撤回要 msg_id，只回 seq 等于让调用方猜。
    pub async fn send_channel_message(
        &self,
        channel: &str,
        actor: &str,
        type_id: &str,
        text: &str,
    ) -> Result<(u64, String)> {
        let channel = validate::id(channel)?;
        let actor = validate::id(actor)?;
        self.require_send_allowed(channel, actor, type_id)?;
        if text.is_empty() || text.chars().count() > 5_000 {
            return Err(PlatformError::BadInput("text required"));
        }
        let store = self
            .message_store
            .as_ref()
            .ok_or_else(|| PlatformError::Mail("message store unavailable".into()))?;
        let msg_id = crate::auth::random_hex(16);
        match store.append(
            (wxlike_message::OwnerType::Channel, channel),
            actor,
            type_id,
            &msg_id,
            text,
        ) {
            // 随机 msg_id 撞车近乎不可能，但撞上了必须说清楚，不能假装发成功
            Ok(Some(seq)) => Ok((seq, msg_id)),
            Ok(None) => Err(PlatformError::Mail("message id collision".into())),
            Err(e) => Err(PlatformError::from(e)),
        }
    }

    /// 撤回频道消息（C2 验收：moderator 能撤他人消息）。
    ///
    /// 本人的走信箱原有 `withdraw`（那里头就一条硬规矩：非作者一律拒绝）；
    /// 撤别人的必须先过 `DeleteAnyMessage`，再走 `withdraw_as`——**权限判断在
    /// 平台层，信箱只多一个"actor 不必是作者"的入口**，且变更事件里记的是
    /// 管理员本人，谁撤的可查（不变式 7）。
    pub fn recall_channel_message(&self, channel: &str, actor: &str, msg_id: &str) -> Result<()> {
        let channel = validate::id(channel)?;
        let actor = validate::id(actor)?;
        let msg_id = validate::id(msg_id)?;
        let store = self
            .message_store
            .as_ref()
            .ok_or_else(|| PlatformError::Mail("message store unavailable".into()))?;
        let owner = (wxlike_message::OwnerType::Channel, channel);
        let sender = store
            .sender_of(owner, msg_id)?
            .ok_or(PlatformError::NotFound)?;
        if sender == actor {
            store.withdraw(owner, msg_id, actor)?;
        } else {
            self.require_in_channel(channel, actor, crate::perms::Perm::DeleteAnyMessage)?;
            store.withdraw_as(owner, msg_id, actor)?;
        }
        self.audit(actor, "channel.recall", channel, msg_id);
        Ok(())
    }

    /// 频道角色名单：所有社区成员都在列，`role` 是**有效**角色（行覆盖 or 继承）。
    pub fn channel_roles(&self, channel: &str, actor: &str) -> Result<Vec<ChannelRoleRow>> {
        let channel = validate::id(channel)?;
        self.effective_channel_role(channel, actor)?
            .ok_or(PlatformError::Forbidden)?;
        let community = self
            .channel_community(channel)?
            .ok_or(PlatformError::NotFound)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT m.user_id, m.role, r.role, r.granted_by
             FROM community_members m
             LEFT JOIN channel_role r ON r.channel_id = ?1 AND r.user_id = m.user_id
             WHERE m.community_id = ?2
             ORDER BY m.joined_at",
        )?;
        let rows = stmt
            .query_map(params![channel, community], |r| {
                let community_role: i64 = r.get(1)?;
                let explicit: Option<String> = r.get(2)?;
                let role = explicit
                    .as_deref()
                    .and_then(ChannelRole::parse)
                    .unwrap_or_else(|| ChannelRole::from_community(community_role));
                Ok(ChannelRoleRow {
                    user_id: r.get(0)?,
                    role: role.as_str().to_string(),
                    inherited: explicit.is_none(),
                    granted_by: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 覆盖某人的频道角色。`owner` 不在可授予之列——频道 owner 就是社区群主，
    /// 转让是另一套流程（与 [`Platform::set_member_role`] 同一道口子）。
    pub fn set_channel_role(
        &self,
        channel: &str,
        actor: &str,
        target: &str,
        role: &str,
    ) -> Result<ChannelRole> {
        let channel = validate::id(channel)?;
        let target = validate::id(target)?;
        let parsed = ChannelRole::parse(role).ok_or(PlatformError::BadInput(
            "role must be moderator|member|guest",
        ))?;
        if parsed == ChannelRole::Owner {
            return Err(PlatformError::BadInput(
                "channel owner comes from the community owner; transfer it instead of granting",
            ));
        }
        self.require_grantor(channel, actor)?;
        let community = self
            .channel_community(channel)?
            .ok_or(PlatformError::NotFound)?;
        let current = self.require_member(&community, target)?;
        if current == ROLE_OWNER {
            return Err(PlatformError::Forbidden);
        }
        self.db().lock().execute(
            "INSERT INTO channel_role(channel_id, user_id, role, granted_by, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel_id, user_id) DO UPDATE
               SET role = excluded.role, granted_by = excluded.granted_by",
            params![
                channel,
                target,
                parsed.as_str(),
                actor,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        self.audit(actor, "channel.set_role", target, role);
        Ok(parsed)
    }

    /// 撤掉覆盖行 = 回到继承（社区角色说了算），不是"把人踢出频道"。
    pub fn reset_channel_role(&self, channel: &str, actor: &str, target: &str) -> Result<()> {
        let channel = validate::id(channel)?;
        let target = validate::id(target)?;
        self.require_grantor(channel, actor)?;
        self.db().lock().execute(
            "DELETE FROM channel_role WHERE channel_id = ?1 AND user_id = ?2",
            params![channel, target],
        )?;
        self.audit(actor, "channel.reset_role", target, "");
        Ok(())
    }

    fn require_grantor(&self, channel: &str, actor: &str) -> Result<ChannelRole> {
        let actor = validate::id(actor)?;
        self.require_in_channel(channel, actor, crate::perms::Perm::ManageRoles)
    }

    pub fn fetch_channel_messages(
        &self,
        channel: &str,
        user: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<(Vec<wxlike_message::MailMessage>, Option<u64>)> {
        let channel = validate::id(channel)?;
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        self.require_member(&community, user)?;
        let store = self.message_store.as_ref().ok_or_else(|| {
            PlatformError::Sqlite(rusqlite::Error::InvalidColumnType(
                0,
                "mailbox".into(),
                rusqlite::types::Type::Null,
            ))
        })?;
        store
            .fetch(
                (wxlike_message::OwnerType::Channel, channel),
                after_seq,
                limit,
            )
            .map_err(|e| e.into())
    }

    // ---- 话题（Zulip 语义：聚合在话题内，不占频道主时间线） ----

    pub fn create_thread(
        &self,
        channel: &str,
        actor: &str,
        root_seq: i64,
        title: &str,
    ) -> Result<String> {
        let channel = validate::id(channel)?;
        let actor = validate::id(actor)?;
        self.require_in_channel(channel, actor, crate::perms::Perm::CreateThread)?;
        if root_seq <= 0 {
            return Err(PlatformError::BadInput(
                "root_seq must reference a channel message",
            ));
        }
        // 根消息必须在频道时间线内存在
        let store = self
            .message_store
            .as_ref()
            .ok_or_else(|| PlatformError::Mail("message store unavailable".into()))?;
        let head = store
            .head_seq((wxlike_message::OwnerType::Channel, channel))
            .map_err(PlatformError::from)?;
        if root_seq as u64 > head {
            return Err(PlatformError::BadInput("root_seq beyond channel timeline"));
        }
        let id = crate::auth::random_hex(8);
        self.db().lock().execute(
            "INSERT INTO threads(id, channel_id, root_seq, title, created_by, tenant_id, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 'default', ?6)",
            params![
                id,
                channel,
                root_seq,
                title,
                actor,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        // 发起者自动订阅
        self.db().lock().execute(
            "INSERT OR IGNORE INTO thread_subs(thread_id, user_id) VALUES(?1, ?2)",
            params![id, actor],
        )?;
        Ok(id)
    }

    pub fn list_threads(&self, channel: &str, user: &str) -> Result<Vec<ThreadRow>> {
        let channel = validate::id(channel)?;
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        self.require_member(&community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, channel_id, root_seq, title, created_by, created_at
             FROM threads WHERE channel_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![channel], |r| {
            Ok(ThreadRow {
                id: r.get(0)?,
                channel_id: r.get(1)?,
                root_seq: r.get(2)?,
                title: r.get(3)?,
                created_by: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 社区内**全部**话题（跨频道），带回复数与"我是否订阅"。
    ///
    /// 这是 C4 的 `/api/community/{id}/threads`：客户端只需要一个入口就能把
    /// "社区里正在讨论什么"列出来，不必先枚举频道再逐个问。
    pub fn community_threads(
        &self,
        community: &str,
        user: &str,
        limit: usize,
    ) -> Result<Vec<CommunityThreadRow>> {
        let community = validate::id(community)?;
        self.require_member(community, user)?;
        let limit = limit.clamp(1, 200) as i64;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.channel_id, ch.name, t.root_seq, t.title, t.created_by, t.created_at,
                    (SELECT COUNT(*) FROM thread_messages m WHERE m.thread_id = t.id),
                    EXISTS(SELECT 1 FROM thread_subs s WHERE s.thread_id = t.id AND s.user_id = ?2)
             FROM threads t
             JOIN channels ch ON ch.id = t.channel_id
             WHERE ch.community_id = ?1
             ORDER BY t.created_at DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![community, user, limit], |r| {
                Ok(CommunityThreadRow {
                    thread_id: r.get(0)?,
                    channel_id: r.get(1)?,
                    channel_name: r.get(2)?,
                    root_seq: r.get(3)?,
                    title: r.get(4)?,
                    created_by: r.get(5)?,
                    created_at: r.get(6)?,
                    replies: r.get(7)?,
                    subscribed: r.get::<_, i64>(8)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn thread_community(&self, thread: &str) -> Result<String> {
        let c = self.db().lock();
        c.query_row(
            "SELECT c.community_id FROM channels c JOIN threads t ON t.channel_id = c.id
             WHERE t.id = ?1",
            params![thread],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// 话题属于哪个频道：推送要挂回频道会话，客户端才认得这条信封。
    fn thread_channel(&self, thread: &str) -> Result<String> {
        let c = self.db().lock();
        c.query_row(
            "SELECT channel_id FROM threads WHERE id = ?1",
            params![thread],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// 话题内回复：落话题自持序列（绝不写频道信箱，不变式 3），再把通知扇给
    /// 订阅且未静音的人（C3）。
    pub async fn send_thread_reply(&self, thread: &str, actor: &str, text: &str) -> Result<i64> {
        let thread = validate::id(thread)?;
        let actor = validate::id(actor)?;
        let channel = self.thread_channel(thread)?;
        self.require_in_channel(&channel, actor, crate::perms::Perm::PostMessage)?;
        if text.is_empty() || text.chars().count() > 5_000 {
            return Err(PlatformError::BadInput("text required"));
        }
        let seq = {
            let c = self.db().lock();
            c.execute(
                "INSERT INTO thread_messages(thread_id, sender, text, created_at)
                 VALUES(?1, ?2, ?3, ?4)",
                params![thread, actor, text, wxlike_core::types::now_ms() as i64],
            )?;
            c.last_insert_rowid()
        };
        let to: Vec<String> = {
            let roster = self.thread_fanout(thread)?;
            roster.into_iter().filter(|u| u.as_str() != actor).collect()
        };
        if !to.is_empty() {
            // `thread.reply` 故意不进消息类型注册表：`mirror_of` 认不出它，
            // 于是这条通知只在会话里飘过，不会落进频道时间线（不变式 3）。
            let mut env = crate::system_envelope(&channel, actor, to, "");
            env.body = wxlike_core::MsgBody::Custom {
                kind: "thread.reply".into(),
                payload: serde_json::json!({
                    "thread_id": thread,
                    "seq": seq,
                    "text": text,
                    "sent_at_ms": wxlike_core::types::now_ms(),
                }),
            };
            self.emit(env).await;
        }
        Ok(seq)
    }

    /// 话题的推送扇出名单（C3 验收项）：订阅 ∩ 仍在社区 ∩ 未静音。
    ///
    /// 信箱的 `MessageStore::members` 回答"谁拥有这个会话"，这一层回答"其中谁
    /// 要被打扰"——退订或静音只把推送拿掉，**读**话题与频道时间线都不受影响。
    pub fn thread_fanout(&self, thread: &str) -> Result<Vec<String>> {
        let thread = validate::id(thread)?;
        let community = self.thread_community(thread)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT s.user_id FROM thread_subs s
             JOIN community_members m ON m.community_id = ?2 AND m.user_id = s.user_id
             WHERE s.thread_id = ?1 AND s.muted = 0
             ORDER BY s.user_id",
        )?;
        let rows = stmt.query_map(params![thread, community], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn fetch_thread_messages(
        &self,
        thread: &str,
        user: &str,
        after_seq: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String, String, i64)>> {
        let thread = validate::id(thread)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        let limit = limit.clamp(1, 200) as i64;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT seq, sender, text, created_at FROM thread_messages
             WHERE thread_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![thread, after_seq, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn subscribe_thread(&self, thread: &str, user: &str) -> Result<()> {
        self.set_thread_subscription(thread, user, false)
    }

    /// 订阅 / 改静音状态。静音的人**仍在订阅名单里**（他要在话题列表里看得见
    /// 这条话题），只是从推送扇出名单里掉出去。
    pub fn set_thread_subscription(&self, thread: &str, user: &str, muted: bool) -> Result<()> {
        let thread = validate::id(thread)?;
        let user = validate::id(user)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        self.db().lock().execute(
            "INSERT INTO thread_subs(thread_id, user_id, muted) VALUES(?1, ?2, ?3)
             ON CONFLICT(thread_id, user_id) DO UPDATE SET muted = excluded.muted",
            params![thread, user, muted as i64],
        )?;
        Ok(())
    }

    pub fn unsubscribe_thread(&self, thread: &str, user: &str) -> Result<()> {
        let thread = validate::id(thread)?;
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        self.db().lock().execute(
            "DELETE FROM thread_subs WHERE thread_id = ?1 AND user_id = ?2",
            params![thread, user],
        )?;
        Ok(())
    }

    /// 订阅名单（含静音标记）——推送用的是 [`Platform::thread_fanout`]。
    pub fn thread_subscribers(&self, thread: &str, user: &str) -> Result<Vec<ThreadSubscription>> {
        let community = self.thread_community(thread)?;
        self.require_member(&community, user)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT user_id, muted FROM thread_subs WHERE thread_id = ?1 ORDER BY user_id",
        )?;
        let rows = stmt.query_map(params![thread], |r| {
            Ok(ThreadSubscription {
                user_id: r.get(0)?,
                muted: r.get::<_, i64>(1)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 频道信箱的成员快照（C3：扇出的第一层）。全量重置，理由同
    /// `groups::sync_mailbox_members`——漏一次增量会让某人永远收不到消息。
    /// 没有信箱时静默跳过（纯控制面测试/演示没有投递）。
    pub fn sync_channel_members(&self, channel: &str) -> Result<()> {
        let Some(community) = self.channel_community(channel)? else {
            return Err(PlatformError::NotFound);
        };
        let Some(store) = self.message_store.as_ref() else {
            return Ok(());
        };
        let ids = self.community_member_ids(&community)?;
        store.reset_member_snapshot((wxlike_message::OwnerType::Channel, channel), &ids)?;
        Ok(())
    }

    /// 社区成员变了 → 该社区下**每个**频道的快照都要重写。
    ///
    /// 成本是 频道数 × 成员数 行插入；社区内频道数受"管理员手工建频道"约束，
    /// 成员变更是低频操作，所以宁可整表重写也不要漏。真到了万人社区 × 几十个
    /// 频道，这里要换成增量成员日志（`append_member_change` 已经有位置）。
    pub fn sync_community_channels(&self, community: &str) -> Result<()> {
        let channels: Vec<String> = {
            let c = self.db().lock();
            let mut stmt = c.prepare("SELECT id FROM channels WHERE community_id = ?1")?;
            let rows = stmt.query_map(params![community], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for channel in channels {
            self.sync_channel_members(&channel)?;
        }
        Ok(())
    }

    fn community_member_ids(&self, community: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT user_id FROM community_members WHERE community_id = ?1")?;
        let rows = stmt.query_map(params![community], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

fn row_channel(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok(ChannelRow {
        id: r.get(0)?,
        community_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        tenant_id: r.get(4)?,
        created_at: r.get(5)?,
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CommunityBody {
    name: String,
    #[serde(default)]
    member_limit: i64,
}

async fn create_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<CommunityBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let id = platform.create_community(&authed.user_id, &body.name, body.member_limit)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "community_id": id })),
    ))
}

async fn my_communities(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.my_communities(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "communities": rows.into_iter().map(|(c, role)| serde_json::json!({
            "id": c.id, "name": c.name, "owner_uid": c.owner_uid,
            "member_limit": c.member_limit, "role": role,
        })).collect::<Vec<_>>()
    })))
}

async fn join_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let state = platform.join_community(&id, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "state": state })))
}

async fn community_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.community_members(&id, &authed.user_id)?;
    Ok(Json(serde_json::json!({
        "members": rows.into_iter().map(|(user, role)| serde_json::json!({
            "user_id": user, "role": role
        })).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
struct ChannelBody {
    name: String,
    kind: String,
}

async fn create_channel(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ChannelBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let channel = platform.create_channel(&id, &authed.user_id, &body.name, &body.kind)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "channel_id": channel })),
    ))
}

async fn list_channels(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.list_channels(&id, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "channels": rows })))
}

#[derive(Deserialize)]
struct ChannelMessageBody {
    text: String,
    /// 消息类型（信箱注册表里的 normal 类）；缺省 `text`。
    #[serde(default)]
    kind: Option<String>,
}

async fn send_channel_message(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ChannelMessageBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (seq, msg_id) = platform
        .send_channel_message(
            &id,
            &authed.user_id,
            body.kind.as_deref().unwrap_or("text"),
            &body.text,
        )
        .await?;
    Ok(Json(serde_json::json!({ "seq": seq, "msg_id": msg_id })))
}

#[derive(Deserialize)]
struct RecallBody {
    msg_id: String,
}

async fn recall_channel_message(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<RecallBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.recall_channel_message(&id, &authed.user_id, &body.msg_id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct FetchQuery {
    #[serde(default)]
    after_seq: u64,
    limit: Option<usize>,
}

async fn fetch_channel_messages(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<FetchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (rows, next) = platform.fetch_channel_messages(
        &id,
        &authed.user_id,
        q.after_seq,
        q.limit.unwrap_or(50),
    )?;
    Ok(Json(serde_json::json!({
        "messages": rows.into_iter().map(|m| serde_json::json!({
            "seq": m.seq, "sender": m.sender, "text": m.payload,
            "kind": m.type_id, "msg_id": m.msg_id, "withdrawn": m.withdrawn,
        })).collect::<Vec<_>>(),
        "next_after_seq": next,
    })))
}

#[derive(Deserialize, Default)]
struct CommunityThreadQuery {
    limit: Option<usize>,
}

async fn community_threads(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<CommunityThreadQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.community_threads(&id, &authed.user_id, q.limit.unwrap_or(50))?;
    Ok(Json(serde_json::json!({ "threads": rows })))
}

#[derive(Deserialize)]
struct ThreadBody {
    root_seq: i64,
    #[serde(default)]
    title: String,
    /// 只在 `/api/community/{id}/threads` 上必填：社区是话题的容器，
    /// 但一条话题始终挂在一个具体频道下。
    #[serde(default)]
    channel_id: Option<String>,
}

async fn create_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ThreadBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let thread = platform.create_thread(&id, &authed.user_id, body.root_seq, &body.title)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "thread_id": thread })),
    ))
}

async fn create_thread_in_community(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ThreadBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let channel = body
        .channel_id
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "channel_id is required when opening a thread from a community",
            )
        })?;
    // 频道必须属于这个社区，否则就是拿社区 id 越权写到别的社区里
    if platform.channel_community(&channel)?.as_deref() != Some(id.as_str()) {
        return Err(PlatformError::NotFound.into());
    }
    let thread = platform.create_thread(&channel, &authed.user_id, body.root_seq, &body.title)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "thread_id": thread })),
    ))
}

async fn list_threads(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.list_threads(&id, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "threads": rows })))
}

#[derive(Deserialize)]
struct ChannelRoleBody {
    user_id: String,
    role: String,
}

async fn list_channel_roles(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.channel_roles(&id, &authed.user_id)?;
    let me = platform
        .effective_channel_role(&id, &authed.user_id)?
        .map(|r| r.as_str());
    Ok(Json(serde_json::json!({
        "channel_id": id, "my_role": me, "roles": rows
    })))
}

async fn set_channel_role(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ChannelRoleBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let role = platform.set_channel_role(&id, &authed.user_id, &body.user_id, &body.role)?;
    Ok(Json(
        serde_json::json!({ "user_id": body.user_id, "role": role.as_str() }),
    ))
}

async fn reset_channel_role(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((id, user)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.reset_channel_role(&id, &authed.user_id, &user)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ThreadReplyBody {
    text: String,
}

async fn send_thread_reply(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<ThreadReplyBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let seq = platform
        .send_thread_reply(&id, &authed.user_id, &body.text)
        .await?;
    Ok(Json(serde_json::json!({ "seq": seq })))
}

#[derive(Deserialize, Default)]
struct ThreadFetchQuery {
    #[serde(default)]
    after_seq: i64,
    limit: Option<usize>,
}

async fn fetch_thread_messages(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<ThreadFetchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.fetch_thread_messages(
        &id,
        &authed.user_id,
        q.after_seq,
        q.limit.unwrap_or(100),
    )?;
    Ok(Json(serde_json::json!({
        "messages": rows.into_iter().map(|(seq, sender, text, _)| serde_json::json!({
            "seq": seq, "sender": sender, "text": text
        })).collect::<Vec<_>>()
    })))
}

async fn subscribe_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.subscribe_thread(&id, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unsubscribe_thread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.unsubscribe_thread(&id, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn thread_subscribers(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.thread_subscribers(&id, &authed.user_id)?;
    let fanout = platform.thread_fanout(&id)?;
    Ok(Json(serde_json::json!({
        "subscribers": rows,
        "fanout": fanout,
    })))
}

#[derive(Deserialize)]
struct SubscriptionBody {
    #[serde(default)]
    muted: bool,
}

/// `PUT /api/threads/{id}/subscribe`：订阅并把静音状态一次写定（幂等）。
async fn set_thread_subscription(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Json(body): Json<SubscriptionBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_thread_subscription(&id, &authed.user_id, body.muted)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perms::Perm;

    /// 矩阵本身要能被钉住：加一个权限点、动一档角色，先在这里红。
    #[test]
    fn channel_role_matrix_tiers_are_strictly_separated() {
        let guest = channel_role_perms(ChannelRole::Guest);
        assert!(guest.contains(&Perm::PostMessage));
        assert!(!guest.contains(&Perm::PostMedia), "guest 不能发附件");
        assert!(!guest.contains(&Perm::PostInAnnounce));

        let member = channel_role_perms(ChannelRole::Member);
        assert!(member.contains(&Perm::PostMedia), "成员能发附件");
        assert!(
            !member.contains(&Perm::DeleteAnyMessage),
            "成员撤不动别人的"
        );

        let moderator = channel_role_perms(ChannelRole::Moderator);
        assert!(
            moderator.contains(&Perm::DeleteAnyMessage),
            "moderator 代撤"
        );
        assert!(moderator.contains(&Perm::PostInAnnounce));
        assert!(
            !moderator.contains(&Perm::ManageRoles),
            "提权只归 owner：被盗号的 moderator 不能自我复制"
        );

        let owner = channel_role_perms(ChannelRole::Owner);
        assert!(owner.contains(&Perm::ManageRoles));
        assert!(owner.contains(&Perm::ManageChannel));
    }

    /// 类型 → 权限点。注册表外的类型不给放行（包括 high_priority/ephemeral：
    /// 它们本来就不该从频道时间线口进来）。
    #[test]
    fn message_type_maps_to_one_perm_point() {
        assert_eq!(perm_for_type("text"), Some(Perm::PostMessage));
        assert_eq!(perm_for_type("system"), Some(Perm::PostMessage));
        assert_eq!(perm_for_type("image"), Some(Perm::PostMedia));
        assert_eq!(
            perm_for_type(wxlike_core::msgmeta::rich_kind::VOICE),
            Some(Perm::PostMedia)
        );
        assert_eq!(perm_for_type("typing"), None, "high_priority 不进时间线");
        assert_eq!(perm_for_type("no.such.type"), None, "未注册的类型不放行");
    }

    #[test]
    fn channel_role_names_round_trip_and_inherit() {
        for role in [
            ChannelRole::Owner,
            ChannelRole::Moderator,
            ChannelRole::Member,
            ChannelRole::Guest,
        ] {
            assert_eq!(ChannelRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(ChannelRole::parse("boss"), None);
        assert_eq!(ChannelRole::from_community(ROLE_OWNER), ChannelRole::Owner);
        assert_eq!(
            ChannelRole::from_community(ROLE_ADMIN),
            ChannelRole::Moderator
        );
        assert_eq!(
            ChannelRole::from_community(ROLE_MEMBER),
            ChannelRole::Member
        );
    }

    /// 扇出的第一层：频道的信箱成员快照必须跟着社区成员走。
    /// 建频道落一次、入社区刷一次——漏掉任何一次，那个人就永远收不到频道消息。
    #[tokio::test]
    async fn sync_channel_members_tracks_the_community_roster() {
        let p = Platform::in_memory().unwrap();
        let owner = p.register("owner", "pw").unwrap();
        let bob = p.register("bob", "pw").unwrap();
        let community = p.create_community(&owner, "社群", 10).unwrap();
        p.join_community(&community, &bob).unwrap();
        let channel = p
            .create_channel(&community, &owner, "综合", "text")
            .unwrap();
        let roster = || {
            p.message_store()
                .unwrap()
                .members((wxlike_message::OwnerType::Channel, &channel))
                .unwrap()
        };
        let mut got = roster();
        got.sort();
        let mut want = vec![owner.clone(), bob.clone()];
        want.sort();
        assert_eq!(got, want, "建频道就把社区成员写进信箱快照");

        let carol = p.register("carol", "pw").unwrap();
        p.join_community(&community, &carol).unwrap();
        assert_eq!(roster().len(), 3, "入社区要刷新该社区下所有频道的快照");

        // 频道不存在时报错，不静默过去（静默降级是本项目明令禁止的）
        let other = p
            .create_channel(&community, &owner, "招聘", "text")
            .unwrap();
        assert!(p.sync_channel_members("deadbeef").is_err());
        assert_eq!(roster().len(), 3, "另一个频道不受影响");
        assert_eq!(
            p.message_store()
                .unwrap()
                .members((wxlike_message::OwnerType::Channel, &other))
                .unwrap()
                .len(),
            3
        );
    }
}
