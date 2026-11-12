//! **handler 层**（`groups_admin`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::groups_admin::{GroupAnnouncement, GroupInvite};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/groups/{group_id}/announcement", put(set_announcement))
        .route("/api/groups/{group_id}/announcement", get(get_announcement))
        .route("/api/groups/{group_id}/mute/{uid}", put(set_mute))
        .route("/api/groups/{group_id}/mute/{uid}", delete(clear_mute))
        .route("/api/groups/{group_id}/mute-all", put(set_all_mute))
        .route("/api/groups/{group_id}/mute-all", get(get_all_mute))
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
    ApiJson(body): ApiJson<AnnouncementBody>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」（groups_admin）：鉴权读 + 公告 upsert。
    let actor = authed.user_id.clone();
    let (text, media_id) = (body.text.clone(), body.media_id.clone());
    offload(&platform, move |p| {
        p.set_announcement(&group, &actor, &text, &media_id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_announcement(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
) -> std::result::Result<Json<GroupAnnouncement>, ApiError> {
    // TODO(ADR-28): 公告读取（`is_member` + `announcement` 两次上锁）仍在 await
    // 上下文里同步跑；与本卡三条样板同类的只读点位，改法照 `set_announcement`。
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
    ApiJson(body): ApiJson<MuteBody>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：单人禁言写（鉴权读 + UPDATE 同一次阻塞）。
    let actor = authed.user_id.clone();
    let until_ms = body.until_ms;
    offload(&platform, move |p| {
        p.set_member_mute(&group, &actor, &uid, until_ms)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group, uid)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：解除禁言写。
    let actor = authed.user_id.clone();
    offload(&platform, move |p| {
        p.clear_member_mute(&group, &actor, &uid)
    })
    .await?;
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
    ApiJson(body): ApiJson<AllMuteBody>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：全员禁言开关写。
    let actor = authed.user_id.clone();
    let on = body.on;
    offload(&platform, move |p| p.set_all_mute(&group, &actor, on)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_all_mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // 与公告读取同一道门：只给成员（非成员 403），不给"群不存在"
    // 与"不是成员"留可区分的信号差异之外的信息。
    // ADR-28 样板「群管理读」：成员校验 + 状态读取同一次阻塞执行，
    // 不在 await 上下文里直接调同步查库函数（区别于公告读取的历史
    // TODO 豁免点位，新点位直接 offload）。非成员以 None 带出，仍由
    // handler 回原来的 403 文案，响应与豁免写法逐字一致。
    let actor = authed.user_id.clone();
    let muted = offload(&platform, move |p| {
        if !p.is_member(&group, &actor)? {
            return Ok(None);
        }
        p.all_muted(&group).map(Some)
    })
    .await?;
    match muted {
        Some(all_muted) => Ok(Json(serde_json::json!({ "all_muted": all_muted }))),
        None => Err(ApiError::new(StatusCode::FORBIDDEN, "members only")),
    }
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
    ApiJson(body): ApiJson<ApprovalBody>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：入群审批开关写。
    let actor = authed.user_id.clone();
    let on = body.on;
    offload(&platform, move |p| p.set_join_approval(&group, &actor, on)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_invite(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
    ApiJson(body): ApiJson<InviteBody>,
) -> std::result::Result<Json<GroupInvite>, ApiError> {
    // ADR-28 样板 ③「群管理写」：邀请 token 写（先鉴权读，再插带过期的行）。
    let actor = authed.user_id.clone();
    let ttl_ms = body.ttl_ms;
    let invite = offload(&platform, move |p| p.create_invite(&group, &actor, ttl_ms)).await?;
    Ok(Json(invite))
}

async fn join_by_token(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(token): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // ADR-28 样板 ③「群管理写」：token 查群 → 免审批直接入群 / 否则排申请，
    // 两条分支的读写都收进同一次阻塞（原来的两次上锁顺序不变）。
    let actor = authed.user_id.clone();
    let state = offload(&platform, move |p| p.join_by_token(&token, &actor)).await?;
    Ok(Json(serde_json::json!({ "state": state })))
}

async fn list_join_requests(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // TODO(ADR-28): 申请列表读取（鉴权 + 一次上锁查行）仍在 await 上下文里同步跑；
    // 与 `get_announcement` 同一类只读点位。
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
    // ADR-28 样板 ③「群管理写」：审批通过 = 更新申请行 + 插入成员行 + 缓存失效。
    let actor = authed.user_id.clone();
    offload(&platform, move |p| {
        p.decide_join_request(&group, &actor, &uid, true)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reject_join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group, uid)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：驳回只动申请行，同样是整段阻塞执行。
    let actor = authed.user_id.clone();
    offload(&platform, move |p| {
        p.decide_join_request(&group, &actor, &uid, false)
    })
    .await?;
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
    ApiJson(body): ApiJson<MentionAllBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let text = platform
        .mention_all(&group, &authed.user_id, &body.text)
        .await?;
    Ok(Json(serde_json::json!({ "text": text, "all": true })))
}
