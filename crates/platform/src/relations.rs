//! P3 关系增强（合同 §3.2 / ADR-24）：拉黑、陌生人消息请求箱、星标/标签/分组。
//!
//! 不变式 9：**陌生人单聊必经 message_requests 流程**——非好友首条消息不产生
//! 会话，落请求箱；接受后发送方获得直发许可；拒绝即封该请求。是否允许陌生
//! 人发起是用户配置（`user_settings.allow_strangers`，默认允许），但流程本身
//! 必须存在、不可绕过。
//!
//! 拉黑是前置闸门：被拉黑者的好友申请与陌生人请求都直接拒绝（hub 侧的投递
//! 收件过滤随 P3-8 信箱接线落地，那里复用 `is_blocked`）。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use wxlike_relations::port::RelationsPort as _;

use crate::{Platform, PlatformState};

/// 关系域错误 → 平台错误。规则在 `wxlike-relations` 里，这里只做类型搬运；
/// HTTP 状态由既有的 `From<PlatformError> for ApiError` 统一决定。
impl From<wxlike_relations::RelError> for PlatformError {
    fn from(err: wxlike_relations::RelError) -> Self {
        use wxlike_relations::RelError as E;
        match err {
            E::BadInput(msg) => Self::BadInput(msg),
            E::Forbidden(_) => Self::Forbidden,
            E::NotFound(_) => Self::NotFound,
            E::Conflict(msg) => Self::Conflict(msg.to_string()),
            E::Db(e) => Self::Sqlite(e),
            E::Json(e) => Self::Codec(e),
        }
    }
}

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/blocks", get(list_blocks))
        .route("/api/blocks/{uid}", put(block))
        .route("/api/blocks/{uid}", delete(unblock))
        .route("/api/stranger/messages", post(send_stranger_message))
        .route("/api/stranger/requests", get(list_stranger_requests))
        .route(
            "/api/stranger/requests/{id}/accept",
            post(accept_stranger_request),
        )
        .route(
            "/api/stranger/requests/{id}/decline",
            post(decline_stranger_request),
        )
        .route(
            "/api/friends/{user_id}",
            axum::routing::patch(patch_friend_meta),
        )
        .route("/api/settings/strangers", get(get_stranger_setting))
        .route("/api/settings/strangers", put(set_stranger_setting))
}

// ---------------------------------------------------------------------------
// blocklist
// ---------------------------------------------------------------------------

impl Platform {
    pub fn block_user(&self, user: &str, target: &str) -> Result<()> {
        let (user, target) = (validate::id(user)?, validate::id(target)?);
        if user == target {
            return Err(PlatformError::BadInput("cannot block yourself"));
        }
        let exists: Option<String> = self
            .db()
            .lock()
            .query_row("SELECT id FROM users WHERE id = ?1", params![target], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        self.db().lock().execute(
            "INSERT OR IGNORE INTO blocks(user_id, blocked_id, created_at) VALUES(?1, ?2, ?3)",
            params![user, target, wxlike_core::types::now_ms() as i64],
        )?;
        Ok(())
    }

    pub fn unblock_user(&self, user: &str, target: &str) -> Result<()> {
        let (user, target) = (validate::id(user)?, validate::id(target)?);
        let changed = self.db().lock().execute(
            "DELETE FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
            params![user, target],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub fn blocked_list(&self, user: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT blocked_id FROM blocks WHERE user_id = ?1 ORDER BY created_at")?;
        let rows = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 拉黑判定（好友申请闸门 + 陌生人请求闸门 + P3-8 hub 收件过滤复用）。
    pub fn is_blocked(&self, user: &str, by: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT 1 FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
            params![by, user],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }
}

// ---------------------------------------------------------------------------
// 陌生人消息请求箱（ADR-24 / 不变式 9）
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StrangerRequest {
    pub id: String,
    pub from_uid: String,
    pub to_uid: String,
    pub text: String,
    pub status: String,
    pub created_at: i64,
}

const MAX_STRANGER_TEXT: usize = 500;

/// 免打扰级别合法值（通知规则与陌生人开关共用校验面）。
pub const MUTE_LEVELS: [&str; 3] = ["all", "mention", "none"];

impl Platform {
    /// 陌生人首条消息：非好友、未拉黑、对方允许 → 请求箱。已好友直接放行
    /// （调用方返回 Accepted 语义）；重复 pending 请求幂等更新文本。
    pub fn send_stranger_message(
        &self,
        from: &str,
        to: &str,
        text: &str,
    ) -> std::result::Result<StrangerState, PlatformError> {
        let (from, to) = (validate::id(from)?, validate::id(to)?);
        if from == to {
            return Err(PlatformError::BadInput("cannot message yourself"));
        }
        if text.len() > MAX_STRANGER_TEXT {
            return Err(PlatformError::BadInput("text too long"));
        }
        if self.are_friends(from, to)? {
            return Ok(StrangerState::AlreadyFriends);
        }
        if self.is_blocked(from, to)? {
            return Err(PlatformError::Forbidden);
        }
        if !self.allow_strangers(to)? {
            return Err(PlatformError::Forbidden);
        }
        let c = self.db().lock();
        // 幂等：同 from→to 已有 pending 则更新文本与时间
        c.execute(
            "INSERT INTO message_requests(id, from_uid, to_uid, text, status, created_at)
             VALUES(?1, ?2, ?3, ?4, 'pending', ?5)
             ON CONFLICT(from_uid, to_uid, status) DO UPDATE SET
               text = excluded.text, created_at = excluded.created_at
             WHERE status = 'pending'",
            params![
                crate::auth::random_hex(8),
                from,
                to,
                text,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(StrangerState::Queued)
    }

    pub fn stranger_requests_in(&self, user: &str) -> Result<Vec<StrangerRequest>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, from_uid, to_uid, text, status, created_at FROM message_requests
             WHERE to_uid = ?1 AND status = 'pending' ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![user], row_request)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn stranger_requests_out(&self, user: &str) -> Result<Vec<StrangerRequest>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT id, from_uid, to_uid, text, status, created_at FROM message_requests
             WHERE from_uid = ?1 ORDER BY created_at DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![user], row_request)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 接受：状态置 accepted，发送方从此可直发（会话许可，不是好友）。
    pub fn accept_stranger_request(&self, user: &str, id: &str) -> Result<()> {
        self.decide_stranger_request(user, id, "accepted")
    }

    /// 拒绝：状态置 declined（该请求关闭；不等于拉黑，可再申请新请求）。
    pub fn decline_stranger_request(&self, user: &str, id: &str) -> Result<()> {
        self.decide_stranger_request(user, id, "declined")
    }

    fn decide_stranger_request(&self, user: &str, id: &str, status: &str) -> Result<()> {
        let id = validate::id(id)?;
        let changed = self.db().lock().execute(
            "UPDATE message_requests SET status = ?1
             WHERE id = ?2 AND to_uid = ?3 AND status = 'pending'",
            params![status, id, user],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    /// 直发许可：好友，或曾接受过来自 `from` 的请求（接受→正常会话语义）。
    pub fn stranger_accepted(&self, from: &str, to: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT 1 FROM message_requests
             WHERE from_uid = ?1 AND to_uid = ?2 AND status = 'accepted'",
            params![from, to],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }

    // ---- 用户设置（是否允许陌生人发起） ----

    pub fn allow_strangers(&self, user: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT allow_strangers FROM user_settings WHERE user_id = ?1",
            params![user],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v != 0)
        .unwrap_or(true))
    }

    pub fn set_allow_strangers(&self, user: &str, allow: bool) -> Result<()> {
        self.db().lock().execute(
            "INSERT INTO user_settings(user_id, allow_strangers) VALUES(?1, ?2)
             ON CONFLICT(user_id) DO UPDATE SET allow_strangers = excluded.allow_strangers",
            params![user, allow as i64],
        )?;
        Ok(())
    }

    // ---- 星标 / 标签 / 分组（friends 表的成员私有元数据） ----

    pub fn patch_friend_meta(
        &self,
        user: &str,
        friend: &str,
        starred: Option<bool>,
        tags: Option<Vec<String>>,
        group: Option<String>,
    ) -> Result<()> {
        let friend = validate::id(friend)?;
        if tags.as_ref().is_some_and(|t| t.len() > 16) {
            return Err(PlatformError::BadInput("too many tags"));
        }
        if group.as_ref().is_some_and(|g| g.chars().count() > 32) {
            return Err(PlatformError::BadInput("group name too long"));
        }
        let tags_json = tags.as_ref().map(serde_json::to_string).transpose()?;
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE friends SET
               starred = COALESCE(?3, starred),
               tags = COALESCE(?4, tags),
               friend_group = COALESCE(?5, friend_group)
             WHERE user_id = ?1 AND friend_id = ?2",
            params![user, friend, starred.map(|s| s as i64), tags_json, group],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrangerState {
    Queued,
    AlreadyFriends,
}

fn row_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<StrangerRequest> {
    Ok(StrangerRequest {
        id: r.get(0)?,
        from_uid: r.get(1)?,
        to_uid: r.get(2)?,
        text: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get(5)?,
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn list_blocks(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "blocked": platform.blocked_list(&authed.user_id)?
    })))
}

async fn block(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(uid): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.block_user(&authed.user_id, &uid)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unblock(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(uid): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.unblock_user(&authed.user_id, &uid)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct StrangerBody {
    to_uid: String,
    text: String,
}

async fn send_stranger_message(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<StrangerBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    match platform.send_stranger_message(&authed.user_id, &body.to_uid, &body.text)? {
        StrangerState::Queued => Ok(Json(
            serde_json::json!({"state": "queued", "detail": "delivered to the request inbox"}),
        )),
        StrangerState::AlreadyFriends => Ok(Json(
            serde_json::json!({"state": "already_friends", "detail": "send via the conversation"}),
        )),
    }
}

async fn list_stranger_requests(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(dir): Query<std::collections::HashMap<String, String>>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = if dir.get("dir").map(|d| d == "out").unwrap_or(false) {
        platform.stranger_requests_out(&authed.user_id)?
    } else {
        platform.stranger_requests_in(&authed.user_id)?
    };
    Ok(Json(serde_json::json!({ "requests": rows })))
}

async fn accept_stranger_request(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.accept_stranger_request(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn decline_stranger_request(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.decline_stranger_request(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct FriendPatch {
    starred: Option<bool>,
    tags: Option<Vec<String>>,
    #[serde(rename = "group")]
    group: Option<String>,
}

async fn patch_friend_meta(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(uid): Path<String>,
    ApiJson(body): ApiJson<FriendPatch>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.patch_friend_meta(&authed.user_id, &uid, body.starred, body.tags, body.group)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_stranger_setting(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "allow_strangers": platform.allow_strangers(&authed.user_id)?
    })))
}

#[derive(Deserialize)]
struct StrangerSetting {
    allow: bool,
}

async fn set_stranger_setting(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<StrangerSetting>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_allow_strangers(&authed.user_id, body.allow)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 跨域只读闸门（G02-02）：聊天/搜索不许自己查 friends / blocks / group_members
// ---------------------------------------------------------------------------

impl Platform {
    /// 这条消息能不能发。`Blocked` 不告诉调用方是哪一侧拉黑的。
    pub fn may_message(&self, from: &str, to: &str) -> Result<wxlike_relations::port::MessageGate> {
        let c = self.db().lock();
        let port = wxlike_relations::port::SqlRelations { conn: &c };
        Ok(port.may_message(from, to)?)
    }

    /// 两人共同的群 id（交集查询，非成员拿不到任何行）。
    pub fn shared_group_ids(&self, a: &str, b: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let port = wxlike_relations::port::SqlRelations { conn: &c };
        Ok(port.shared_groups(a, b)?)
    }
}
