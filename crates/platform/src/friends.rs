//! **handler 层**（`friends`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::validate;
use crate::{direct_conv_id, system_envelope, PlatformState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};

use crate::domain::friends::{display_name, FriendRow, RequestRow};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/friends/requests", post(create_request))
        .route("/api/friends/requests/{req_id}/accept", post(accept))
        .route("/api/friends/requests/{req_id}/decline", post(decline))
        .route("/api/friends/requests/incoming", get(incoming))
        .route("/api/friends/requests/outgoing", get(outgoing))
        .route("/api/friends", get(list_friends))
        .route("/api/friends/changes", get(changes))
        .route("/api/friends/{user_id}/remark", put(set_remark))
        .route("/api/friends/{user_id}", delete(remove_friend))
}

#[derive(serde::Deserialize)]
pub struct RequestBody {
    pub to_user_id: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct RemarkBody {
    pub remark: String,
}

#[derive(serde::Deserialize)]
pub struct ChangesQuery {
    #[serde(default)]
    since: Option<i64>,
}

async fn create_request(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<RequestBody>,
) -> std::result::Result<(StatusCode, Json<RequestRow>), ApiError> {
    // ADR-28 样板 ②「关系写」：已改造。
    //
    // 改之前：`platform.send_friend_request(..)?` 直接在 handler 的 await 上下文里
    // 拿 `db().lock()` 跑完整事务，占住一个 tokio worker 直到 SQL 结束。
    // 现在：整段（拉黑闸门 → 目标存在 → 复用/插入 → 双向 contact_log）作为一个
    // 整体进 `spawn_blocking`——事务边界一点没动，动的只是它跑在哪根线程上。
    let message = body.message.clone().unwrap_or_default();
    let to = body.to_user_id.clone();
    let me = authed.user_id.clone();
    let note_text = message.clone();
    let (req_id, to_uid, from_name) = offload(&platform, move |p| {
        let (req_id, to_uid) = p.send_friend_request(&me, &to, &note_text)?;
        Ok((req_id, to_uid, display_name(p, &me)))
    })
    .await?;
    let note = if message.is_empty() {
        format!("{from_name} 请求添加你为好友")
    } else {
        format!("{from_name} 请求添加你为好友：{message}")
    };
    let env = system_envelope(
        &direct_conv_id(&authed.user_id, &to_uid),
        &authed.user_id,
        vec![to_uid.clone()],
        &note,
    );
    platform.emit(env).await;
    let row = offload(&platform, move |p| p.load_request(&req_id)).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn accept(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(req_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // ADR-28 样板 ②「关系写」：读请求 → 鉴权 → 好友双向行 + 两条 contact_log
    // 在同一个事务里提交，整段搬到阻塞线程池；handler 只 `await`。
    let me = authed.user_id.clone();
    let (req, my_name) = offload(&platform, move |p| {
        let req = p.accept_request(&me, &req_id)?;
        Ok((req, display_name(p, &me)))
    })
    .await?;
    let env = system_envelope(
        &direct_conv_id(&req.from_uid, &req.to_uid),
        &authed.user_id,
        vec![req.from_uid.clone()],
        &format!("{my_name} 已同意你的好友请求"),
    );
    platform.emit(env).await;
    Ok(Json(serde_json::json!({
        "friend_id": req.from_uid,
        "status": "accepted",
    })))
}

async fn decline(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(req_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // ADR-28 样板 ②「关系写」：同 `accept`，decline 也是双向日志 + 状态位一个事务。
    let me = authed.user_id.clone();
    let (req, my_name) = offload(&platform, move |p| {
        let req = p.decline_request(&me, &req_id)?;
        Ok((req, display_name(p, &me)))
    })
    .await?;
    let env = system_envelope(
        &direct_conv_id(&req.from_uid, &req.to_uid),
        &authed.user_id,
        vec![req.from_uid.clone()],
        &format!("{my_name} 拒绝了你的好友请求"),
    );
    platform.emit(env).await;
    Ok(Json(serde_json::json!({ "status": "declined" })))
}

// TODO(ADR-28): 通讯录的四个读接口仍在 await 上下文里同步查库（只读、本卡三条
// 样板热点之外的点位）。改法与 `accept` 一致：`offload(&platform, move |p| ..)`。
async fn incoming(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<RequestRow>>, ApiError> {
    // TODO(ADR-28): 见上面的模块级说明——同步查库留在 await 上下文里。
    Ok(Json(platform.incoming_requests(&authed.user_id)?))
}

async fn outgoing(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<RequestRow>>, ApiError> {
    // TODO(ADR-28): 同步查库留在 await 上下文里（见 `incoming` 上方说明）。
    Ok(Json(platform.outgoing_requests(&authed.user_id)?))
}

async fn list_friends(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<FriendRow>>, ApiError> {
    // TODO(ADR-28): 同步查库留在 await 上下文里（见 `incoming` 上方说明）。
    Ok(Json(platform.friends(&authed.user_id)?))
}

async fn set_remark(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(user_id): Path<String>,
    ApiJson(body): ApiJson<RemarkBody>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ②「关系写」：单条 UPDATE 也照样走阻塞池——热点判断不看语句多少，
    // 看它是不是在 await 上下文里占线程。
    let me = authed.user_id.clone();
    let remark = body.remark.clone();
    offload(&platform, move |p| p.set_remark(&me, &user_id, &remark)).await?;
    Ok(StatusCode::OK)
}

async fn remove_friend(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(user_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ②「关系写」：`are_friends` → 双行删除 + 两条日志（一个事务）→
    // 取显示名，三段同步 SQL 合成一次阻塞执行，中间不插 `await`。
    let other = validate::id(&user_id).map_err(ApiError::from)?.to_string();
    let me = authed.user_id.clone();
    let target = other.clone();
    let (was_friend, my_name) = offload(&platform, move |p| {
        let was_friend = p.are_friends(&me, &target)?;
        p.remove_friend(&me, &target)?;
        Ok((was_friend, display_name(p, &me)))
    })
    .await?;
    if was_friend {
        let env = system_envelope(
            &direct_conv_id(&authed.user_id, &other),
            &authed.user_id,
            vec![other.clone()],
            &format!("{my_name} 已将你从通讯录移除"),
        );
        platform.emit(env).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn changes(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ChangesQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // TODO(ADR-28): 增量同步两次上锁（changes + changes_head）都在 await 上下文里
    // 跑（见 `incoming` 上方说明）。
    let since = q.since.unwrap_or(0).max(0);
    let rows = platform.changes(&authed.user_id, since, 200)?;
    let head = platform.changes_head(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "changes": rows,
        "cursor": head,
        "has_more": head > rows.last().map(|r| r.seq).unwrap_or(since),
    })))
}
