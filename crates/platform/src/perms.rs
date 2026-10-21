//! 权限矩阵（**参照 Rocket.Chat 的 roles × permissions 模型**）。
//!
//! Rocket.Chat 把权限做成"角色持有一组 permission 字符串"，端点只问
//! `can(user, 'manage-channel')`，不关心角色叫什么。我们沿用这个分层：
//!
//! * `Perm` —— 权限点，字符串名与 Rocket.Chat 对齐（便于对照与将来做管理后台）；
//! * `role_perms(role)` —— 角色 → 权限集，纯函数，好测；
//! * `Platform::can/require_perm` —— 端点侧唯一入口。
//!
//! 与既有 `require_manager` 的关系：`require_manager` 是"是不是管理员"的粗判，
//! 这里替换成按权限点判。老的粗判仍保留给 live 等未迁移模块，但新代码一律用
//! 权限点——**判角色会让"加一个权限"变成改所有端点**。
//!
//! 角色是社区内的数字（`ROLE_MEMBER=0`、`ROLE_OWNER=1`、`ROLE_ADMIN=2`，
//! 注意 owner 比 admin 小，别用大小比较判强弱）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::Authed;
use crate::communities::{ROLE_ADMIN, ROLE_MEMBER, ROLE_OWNER};
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

/// 权限点。字符串名与 Rocket.Chat 的 permission 命名保持一致或可直接对照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perm {
    /// 在普通频道发消息
    PostMessage,
    /// 在公告频道发消息（Rocket.Chat: `post-readonly` 的反面）
    PostInAnnounce,
    /// 发带附件的消息（图/文件/语音/视频/贴纸）——Zone C · C2 的消息类型闸门
    PostMedia,
    /// 在频道里开新话题
    CreateThread,
    /// 新建频道
    CreateChannel,
    /// 改名/改类型/删频道
    ManageChannel,
    /// 加人/踢人
    ManageMembers,
    /// 授予或撤销管理员
    ManageRoles,
    /// 撤回他人消息
    DeleteAnyMessage,
    /// 置顶消息
    PinMessage,
    /// 通话录制
    RecordCall,
}

impl Perm {
    pub fn as_str(self) -> &'static str {
        match self {
            Perm::PostMessage => "post-message",
            Perm::PostInAnnounce => "post-announce",
            Perm::PostMedia => "post-media",
            Perm::CreateThread => "create-thread",
            Perm::CreateChannel => "create-channel",
            Perm::ManageChannel => "manage-channel",
            Perm::ManageMembers => "manage-members",
            Perm::ManageRoles => "manage-roles",
            Perm::DeleteAnyMessage => "delete-any-message",
            Perm::PinMessage => "pin-message",
            Perm::RecordCall => "record-call",
        }
    }

    fn parse(s: &str) -> Option<Perm> {
        Some(match s {
            "post-message" => Perm::PostMessage,
            "post-announce" => Perm::PostInAnnounce,
            "post-media" => Perm::PostMedia,
            "create-thread" => Perm::CreateThread,
            "create-channel" => Perm::CreateChannel,
            "manage-channel" => Perm::ManageChannel,
            "manage-members" => Perm::ManageMembers,
            "manage-roles" => Perm::ManageRoles,
            "delete-any-message" => Perm::DeleteAnyMessage,
            "pin-message" => Perm::PinMessage,
            "record-call" => Perm::RecordCall,
            _ => return None,
        })
    }
}

/// 成员的权限集。
///
/// * 群主：全部；
/// * 管理员：除"授予管理员"外全部——**提权必须群主亲自做**，否则一个被
///   盗号的管理员可以自我复制；
/// * 成员：只能发消息；公告频道连发消息都不行。
pub fn role_perms(role: i64) -> &'static [Perm] {
    const MEMBER: &[Perm] = &[Perm::PostMessage];
    const ADMIN: &[Perm] = &[
        Perm::PostMessage,
        Perm::PostInAnnounce,
        Perm::CreateChannel,
        Perm::ManageChannel,
        Perm::ManageMembers,
        Perm::DeleteAnyMessage,
        Perm::PinMessage,
        Perm::RecordCall,
    ];
    const OWNER: &[Perm] = &[
        Perm::PostMessage,
        Perm::PostInAnnounce,
        Perm::CreateChannel,
        Perm::ManageChannel,
        Perm::ManageMembers,
        Perm::ManageRoles,
        Perm::DeleteAnyMessage,
        Perm::PinMessage,
        Perm::RecordCall,
    ];
    match role {
        ROLE_OWNER => OWNER,
        ROLE_ADMIN => ADMIN,
        ROLE_MEMBER => MEMBER,
        _ => MEMBER,
    }
}

impl Platform {
    /// 我在这个社区的有效权限（未加入返回空集，而不是报错——UI 要能显示灰按钮）。
    pub fn effective_perms(&self, community: &str, user: &str) -> Result<Vec<&'static str>> {
        let Ok(role) = self.require_member(community, user) else {
            return Ok(Vec::new());
        };
        Ok(role_perms(role).iter().map(|p| p.as_str()).collect())
    }

    /// 我是否持有某个权限点。
    pub fn can(&self, community: &str, user: &str, perm: Perm) -> Result<bool> {
        match self.require_member(community, user) {
            Ok(role) => Ok(role_perms(role).contains(&perm)),
            Err(PlatformError::Forbidden) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn require_perm(&self, community: &str, user: &str, perm: Perm) -> Result<i64> {
        let role = self.require_member(community, user)?;
        if role_perms(role).contains(&perm) {
            Ok(role)
        } else {
            Err(PlatformError::Forbidden)
        }
    }

    /// 频道级权限判定：先由频道找到社区，再按社区角色判。
    pub(crate) fn require_channel_perm(
        &self,
        channel: &str,
        user: &str,
        perm: Perm,
    ) -> Result<String> {
        let community = self
            .channel_community(channel)?
            .ok_or(PlatformError::NotFound)?;
        self.require_perm(&community, user, perm)?;
        Ok(community)
    }

    // ---- 管理动作（端点直接调这些） ----

    /// 改频道名/类型。`kind` 只接受 `CHANNEL_KINDS` 里的值。
    pub fn update_channel(
        &self,
        channel: &str,
        actor: &str,
        name: Option<&str>,
        kind: Option<&str>,
    ) -> Result<()> {
        let channel = validate::id(channel)?.to_string();
        let community = self.require_channel_perm(&channel, actor, Perm::ManageChannel)?;
        let name = match name {
            Some(n) => Some(validate::name(n)?.to_string()),
            None => None,
        };
        if let Some(k) = kind {
            if !crate::communities::CHANNEL_KINDS.contains(&k) {
                return Err(PlatformError::BadInput("kind"));
            }
        }
        {
            let c = self.db().lock();
            if let Some(n) = name {
                c.execute(
                    "UPDATE channels SET name = ?1 WHERE id = ?2",
                    rusqlite::params![n, channel],
                )?;
            }
            if let Some(k) = kind {
                c.execute(
                    "UPDATE channels SET kind = ?1 WHERE id = ?2",
                    rusqlite::params![k, channel],
                )?;
            }
        }
        self.audit(actor, "channel.update", &channel, "");
        let _ = community;
        Ok(())
    }

    /// 删频道（频道消息随外键级联删除，见 schema）。
    pub fn delete_channel(&self, channel: &str, actor: &str) -> Result<()> {
        let channel = validate::id(channel)?.to_string();
        let community = self.require_channel_perm(&channel, actor, Perm::ManageChannel)?;
        {
            let c = self.db().lock();
            c.execute(
                "DELETE FROM channels WHERE id = ?1",
                rusqlite::params![channel],
            )?;
        }
        self.audit(actor, "channel.delete", &channel, "");
        let _ = community;
        Ok(())
    }

    /// 改成员角色：`admin` / `member`。只有群主能授予或撤销管理员。
    pub fn set_member_role(
        &self,
        community: &str,
        actor: &str,
        target: &str,
        role: &str,
    ) -> Result<()> {
        let community = validate::id(community)?.to_string();
        let target = validate::id(target)?.to_string();
        let new_role = match role {
            "admin" => ROLE_ADMIN,
            "member" => ROLE_MEMBER,
            _ => return Err(PlatformError::BadInput("role")),
        };
        if new_role == ROLE_ADMIN {
            // 提权：必须群主本人，且不能把群主降级（要走转让流程，另行实现）
            self.require_perm(&community, actor, Perm::ManageRoles)?;
        } else {
            self.require_perm(&community, actor, Perm::ManageMembers)?;
        }
        let current = self.require_member(&community, &target)?;
        if current == ROLE_OWNER {
            return Err(PlatformError::Forbidden);
        }
        if current == ROLE_ADMIN && new_role == ROLE_MEMBER {
            // 管理员之间互相降级同样要群主，否则会变成内斗
            self.require_perm(&community, actor, Perm::ManageRoles)?;
        }
        {
            let c = self.db().lock();
            c.execute(
                "UPDATE community_members SET role = ?1 WHERE community_id = ?2 AND user_id = ?3",
                rusqlite::params![new_role, community, target],
            )?;
        }
        self.audit(actor, "community.set_role", &target, role);
        Ok(())
    }

    /// 踢出社区：不能踢群主；管理员之间不能互踢（要群主）。
    pub fn kick_member(&self, community: &str, actor: &str, target: &str) -> Result<()> {
        let community = validate::id(community)?.to_string();
        let target = validate::id(target)?.to_string();
        let actor_role = self.require_perm(&community, actor, Perm::ManageMembers)?;
        let target_role = self.require_member(&community, &target)?;
        if target_role == ROLE_OWNER {
            return Err(PlatformError::Forbidden);
        }
        if target_role == ROLE_ADMIN && actor_role != ROLE_OWNER {
            return Err(PlatformError::Forbidden);
        }
        {
            let c = self.db().lock();
            c.execute(
                "DELETE FROM community_members WHERE community_id = ?1 AND user_id = ?2",
                rusqlite::params![community, target],
            )?;
        }
        self.audit(actor, "community.kick", &target, "");
        Ok(())
    }
}

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/communities/{id}/permissions", get(my_perms))
        .route("/api/communities/{id}/members/{user}/role", post(set_role))
        .route("/api/communities/{id}/members/{user}", delete(kick))
        .route("/api/channels/{id}", patch(update_channel))
        .route("/api/channels/{id}", delete(delete_channel))
}

#[derive(Serialize)]
struct PermsReply {
    community_id: String,
    perms: Vec<&'static str>,
}

async fn my_perms(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PermsReply>, ApiError> {
    let perms = platform.effective_perms(&id, &authed.user_id)?;
    Ok(Json(PermsReply {
        community_id: id,
        perms,
    }))
}

#[derive(serde::Deserialize)]
struct RoleBody {
    role: String,
}

async fn set_role(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((id, user)): Path<(String, String)>,
    ApiJson(body): ApiJson<RoleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_member_role(&id, &authed.user_id, &user, &body.role)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn kick(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((id, user)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.kick_member(&id, &authed.user_id, &user)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize, Default)]
struct ChannelPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

async fn update_channel(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ChannelPatch>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.update_channel(
        &id,
        &authed.user_id,
        body.name.as_deref(),
        body.kind.as_deref(),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_channel(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_channel(&id, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 供其它模块把字符串权限点翻成枚举（管理后台/审计回放会用到）。
pub fn parse_perm(s: &str) -> Option<Perm> {
    Perm::parse(s)
}
