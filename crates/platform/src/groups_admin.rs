//! P3 群管理补齐（合同 §3.3）：公告、禁言（全员/单人/定时解除）、
//! 入群审批 + 邀请 token（带过期）、@所有人（owner/admin 限定）。
//!
//! 与 groups.rs 的关系：同一个 `Platform` 的扩展 impl（模块化单体的 crate
//! 内分文件），路由独立注册。禁言的**发送侧拦截**由 hub 接线复用
//! [`Platform::is_muted`]（P3-8）；此处先落存储面与 HTTP 面。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/groups/{group_id}/announcement", put(set_announcement))
        .route("/api/groups/{group_id}/announcement", get(get_announcement))
        .route("/api/groups/{group_id}/mute/{uid}", put(set_mute))
        .route("/api/groups/{group_id}/mute/{uid}", delete(clear_mute))
        .route("/api/groups/{group_id}/mute-all", put(set_all_mute))
        .route("/api/groups/{group_id}/approval", put(set_approval))
        .route("/api/groups/{group_id}/invites", post(create_invite))
        .route("/api/groups/join/{token}", post(join_by_token))
        .route(
            "/api/groups/{group_id}/join-requests",
            get(list_join_requests),
        )
        .route(
            "/api/groups/{group_id}/join-requests/{uid}/approve",
            post(approve_join),
        )
        .route(
            "/api/groups/{group_id}/join-requests/{uid}/reject",
            post(reject_join),
        )
        .route("/api/groups/{group_id}/mention-all", post(mention_all))
}

// ---------------------------------------------------------------------------
// 公告
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GroupAnnouncement {
    pub text: String,
    pub media_id: String,
    pub pinned_by: String,
    pub updated_at: i64,
}

impl Platform {
    pub fn set_announcement(
        &self,
        group: &str,
        actor: &str,
        text: &str,
        media_id: &str,
    ) -> Result<()> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        if text.chars().count() > 2_000 {
            return Err(PlatformError::BadInput("announcement too long"));
        }
        self.db().lock().execute(
            "INSERT INTO group_announcement(group_id, text, media_id, pinned_by, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(group_id) DO UPDATE SET
               text = excluded.text, media_id = excluded.media_id,
               pinned_by = excluded.pinned_by, updated_at = excluded.updated_at",
            params![
                group,
                text,
                media_id,
                actor,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(())
    }

    pub fn announcement(&self, group: &str) -> Result<Option<GroupAnnouncement>> {
        let group = validate::id(group)?;
        let c = self.db().lock();
        let row = c
            .query_row(
                "SELECT text, media_id, pinned_by, updated_at FROM group_announcement
                 WHERE group_id = ?1",
                params![group],
                |r| {
                    Ok(GroupAnnouncement {
                        text: r.get(0)?,
                        media_id: r.get(1)?,
                        pinned_by: r.get(2)?,
                        updated_at: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}

// ---------------------------------------------------------------------------
// 禁言（全员 + 单人定时）
// ---------------------------------------------------------------------------

impl Platform {
    pub fn set_member_mute(
        &self,
        group: &str,
        actor: &str,
        target: &str,
        until_ms: u64,
    ) -> Result<()> {
        let (group, target) = (validate::id(group)?, validate::id(target)?);
        self.assert_manager(group, actor)?;
        if !self.is_member(group, target)? {
            return Err(PlatformError::NotFound);
        }
        let role = self.role_of(group, target)?;
        if role == "owner" {
            return Err(PlatformError::BadInput("the owner cannot be muted"));
        }
        self.db().lock().execute(
            "INSERT INTO group_mutes(group_id, user_id, until_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(group_id, user_id) DO UPDATE SET until_ms = excluded.until_ms",
            params![group, target, until_ms as i64],
        )?;
        Ok(())
    }

    pub fn clear_member_mute(&self, group: &str, actor: &str, target: &str) -> Result<()> {
        let (group, target) = (validate::id(group)?, validate::id(target)?);
        self.assert_manager(group, actor)?;
        let changed = self.db().lock().execute(
            "DELETE FROM group_mutes WHERE group_id = ?1 AND user_id = ?2",
            params![group, target],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub fn set_all_mute(&self, group: &str, actor: &str, on: bool) -> Result<()> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        self.db().lock().execute(
            "UPDATE groups SET all_muted = ?1 WHERE id = ?2",
            params![on as i64, group],
        )?;
        Ok(())
    }

    /// 发送侧裁决（hub 接线复用）：全员禁言对 owner/admin 无效；单人禁言
    /// 按 until_ms 定时解除（到点自然放行，无需 sweeper）。
    pub fn is_muted(&self, group: &str, user: &str) -> Result<bool> {
        let group = validate::id(group)?;
        let role = self.role_of(group, user)?;
        if role == "owner" || role == "admin" {
            return Ok(false);
        }
        let all: i64 = self.db().lock().query_row(
            "SELECT all_muted FROM groups WHERE id = ?1",
            params![group],
            |r| r.get(0),
        )?;
        if all != 0 {
            return Ok(true);
        }
        let until: Option<i64> = self
            .db()
            .lock()
            .query_row(
                "SELECT until_ms FROM group_mutes WHERE group_id = ?1 AND user_id = ?2",
                params![group, user],
                |r| r.get(0),
            )
            .optional()?;
        Ok(until
            .map(|u| u > wxlike_core::types::now_ms() as i64)
            .unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------
// 邀请 token + 入群审批
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GroupInvite {
    pub token: String,
    pub group_id: String,
    pub created_by: String,
    pub expires_at: i64,
}

impl Platform {
    /// token/审批路径的成员直入：与 add_members 同一套不变量（存在性、上限、
    /// 缓存失效），但**不做 actor 管理员断言**——授权来自有效邀请/批准本身。
    fn add_member_direct(&self, group: &str, user: &str) -> Result<()> {
        let (group, user) = (validate::id(group)?, validate::id(user)?);
        if !self.group_exists(group)? {
            return Err(PlatformError::NotFound);
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let current: i64 = c.query_row(
            "SELECT COUNT(*) FROM group_members WHERE group_id = ?1",
            params![group],
            |r| r.get(0),
        )?;
        if current as usize >= crate::groups::MAX_GROUP_MEMBERS {
            return Err(PlatformError::BadInput("group member limit exceeded"));
        }
        let exists: Option<String> = c
            .query_row("SELECT id FROM users WHERE id = ?1", params![user], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        c.execute(
            "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at)
             VALUES(?1, ?2, ?3, '', ?4)
             ON CONFLICT(group_id, user_id) DO NOTHING",
            params![group, user, crate::groups::ROLE_MEMBER, now],
        )?;
        drop(c);
        self.invalidate_group(group);
        Ok(())
    }

    /// 审批开关（owner/admin 切换；审批开的群 token 入群转请求）。
    pub fn set_join_approval(&self, group: &str, actor: &str, on: bool) -> Result<()> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        self.db().lock().execute(
            "UPDATE groups SET approval = ?1 WHERE id = ?2",
            params![on as i64, group],
        )?;
        Ok(())
    }

    pub fn create_invite(&self, group: &str, actor: &str, ttl_ms: u64) -> Result<GroupInvite> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        let token = crate::auth::random_hex(16);
        let expires = wxlike_core::types::now_ms() as i64 + ttl_ms as i64;
        self.db().lock().execute(
            "INSERT INTO group_invites(token, group_id, created_by, expires_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![token, group, actor, expires],
        )?;
        Ok(GroupInvite {
            token,
            group_id: group.to_string(),
            created_by: actor.to_string(),
            expires_at: expires,
        })
    }

    /// token 入群：审批开 → pending 请求；审批关 → 直接入群。
    pub fn join_by_token(&self, token: &str, user: &str) -> Result<&'static str> {
        let token = validate::id(token)?;
        let user = validate::id(user)?;
        // scope the guard: is_member/add_members re-lock the same non-reentrant
        // Mutex — holding it across those calls deadlocked the caller's thread
        let row: Option<(String, i64)> = {
            let c = self.db().lock();
            c.query_row(
                "SELECT group_id, expires_at FROM group_invites WHERE token = ?1",
                params![token],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        };
        let Some((group, expires_at)) = row else {
            return Err(PlatformError::NotFound);
        };
        if expires_at <= wxlike_core::types::now_ms() as i64 {
            return Err(PlatformError::BadInput("invite expired"));
        }
        if self.is_member(&group, user)? {
            return Ok("already_member");
        }
        let approval: i64 = self.db().lock().query_row(
            "SELECT approval FROM groups WHERE id = ?1",
            params![group],
            |r| r.get(0),
        )?;
        if approval == 0 {
            self.add_member_direct(&group, user)?;
            Ok("joined")
        } else {
            self.db().lock().execute(
                "INSERT INTO group_join_requests(group_id, user_id, status, requested_at)
                 VALUES(?1, ?2, 'pending', ?3)
                 ON CONFLICT(group_id, user_id) DO UPDATE SET status = 'pending',
                   requested_at = excluded.requested_at",
                params![group, user, wxlike_core::types::now_ms() as i64],
            )?;
            Ok("pending")
        }
    }

    pub fn join_requests(&self, group: &str, actor: &str) -> Result<Vec<(String, String)>> {
        let group = validate::id(group)?;
        self.assert_manager(group, actor)?;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT user_id, requested_at FROM group_join_requests
             WHERE group_id = ?1 AND status = 'pending' ORDER BY requested_at",
        )?;
        let rows = stmt.query_map(params![group], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.to_string()))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn decide_join_request(
        &self,
        group: &str,
        actor: &str,
        user: &str,
        approve: bool,
    ) -> Result<()> {
        let (group, user) = (validate::id(group)?, validate::id(user)?);
        self.assert_manager(group, actor)?;
        if approve {
            self.add_member_direct(group, user)?;
        }
        let status = if approve { "approved" } else { "rejected" };
        let changed = self.db().lock().execute(
            "UPDATE group_join_requests SET status = ?1
             WHERE group_id = ?2 AND user_id = ?3 AND status = 'pending'",
            params![status, group, user],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// @所有人（owner/admin 限定）
// ---------------------------------------------------------------------------

impl Platform {
    pub fn can_mention_all(&self, group: &str, user: &str) -> Result<bool> {
        let role = self.role_of(group, user)?;
        Ok(role == "owner" || role == "admin")
    }

    /// @所有人：manager 专属。构造 mention Custom 帧并经 EventSink 扇给全体
    /// 成员（未指定 user_ids 语义 = all），帧内带 all 标记。
    pub async fn mention_all(&self, group: &str, actor: &str, text: &str) -> Result<String> {
        let group = validate::id(group)?;
        if !self.can_mention_all(group, actor)? {
            return Err(PlatformError::Forbidden);
        }
        if text.is_empty() || text.chars().count() > 2_000 {
            return Err(PlatformError::BadInput("text required"));
        }
        let members = self.cached_members(group)?.unwrap_or_default();
        let env = crate::system_envelope(group, actor, members.clone(), "");
        let mut env = env;
        env.body = wxlike_core::MsgBody::Custom {
            kind: "mention".into(),
            payload: serde_json::json!({ "text": text, "all": true, "user_ids": members }),
        };
        self.emit(env).await;
        Ok(text.to_string())
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AnnouncementBody {
    text: String,
    #[serde(default)]
    media_id: String,
}

async fn set_announcement(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
    Json(body): Json<AnnouncementBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_announcement(&group, &authed.user_id, &body.text, &body.media_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_announcement(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
) -> std::result::Result<Json<GroupAnnouncement>, ApiError> {
    if !platform.is_member(&group, &authed.user_id)? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "members only"));
    }
    platform
        .announcement(&group)?
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no announcement"))
}

#[derive(Deserialize)]
struct MuteBody {
    /// 绝对时间戳（ms）；已过期即视为未禁言（定时解除免 sweeper）。
    until_ms: u64,
}

async fn set_mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group, uid)): Path<(String, String)>,
    Json(body): Json<MuteBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_member_mute(&group, &authed.user_id, &uid, body.until_ms)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group, uid)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.clear_member_mute(&group, &authed.user_id, &uid)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AllMuteBody {
    on: bool,
}

async fn set_all_mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
    Json(body): Json<AllMuteBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_all_mute(&group, &authed.user_id, body.on)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct InviteBody {
    ttl_ms: u64,
}

#[derive(Deserialize)]
struct ApprovalBody {
    on: bool,
}

async fn set_approval(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
    Json(body): Json<ApprovalBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_join_approval(&group, &authed.user_id, body.on)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_invite(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
    Json(body): Json<InviteBody>,
) -> std::result::Result<Json<GroupInvite>, ApiError> {
    Ok(Json(platform.create_invite(
        &group,
        &authed.user_id,
        body.ttl_ms,
    )?))
}

async fn join_by_token(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(token): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let state = platform.join_by_token(&token, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "state": state })))
}

async fn list_join_requests(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.join_requests(&group, &authed.user_id)?;
    Ok(Json(serde_json::json!({
        "requests": rows.into_iter().map(|(user, at)| serde_json::json!({
            "user_id": user, "requested_at": at
        })).collect::<Vec<_>>()
    })))
}

async fn approve_join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group, uid)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.decide_join_request(&group, &authed.user_id, &uid, true)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reject_join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group, uid)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.decide_join_request(&group, &authed.user_id, &uid, false)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MentionAllBody {
    text: String,
}

async fn mention_all(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
    Json(body): Json<MentionAllBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let text = platform
        .mention_all(&group, &authed.user_id, &body.text)
        .await?;
    Ok(Json(serde_json::json!({ "text": text, "all": true })))
}
