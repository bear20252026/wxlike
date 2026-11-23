//! **handler 层**（`relations`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::relations::StrangerState;

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

async fn list_blocks(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let blocked = offload(&platform, move |p| p.blocked_list(&actor)).await?;
    Ok(Json(serde_json::json!({ "blocked": blocked })))
}

async fn block(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(uid): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.block_user(&actor, &uid)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unblock(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(uid): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.unblock_user(&actor, &uid)).await?;
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
    let actor = authed.user_id.clone();
    let to_uid = body.to_uid;
    let text = body.text;
    let state = offload(&platform, move |p| {
        p.send_stranger_message(&actor, &to_uid, &text)
    })
    .await?;
    match state {
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
    let actor = authed.user_id.clone();
    let out = dir.get("dir").map(|d| d == "out").unwrap_or(false);
    let rows = offload(&platform, move |p| {
        if out {
            p.stranger_requests_out(&actor)
        } else {
            p.stranger_requests_in(&actor)
        }
    })
    .await?;
    Ok(Json(serde_json::json!({ "requests": rows })))
}

async fn accept_stranger_request(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.accept_stranger_request(&actor, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn decline_stranger_request(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.decline_stranger_request(&actor, &id)).await?;
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
    let actor = authed.user_id.clone();
    let starred = body.starred;
    let tags = body.tags;
    let group = body.group;
    offload(&platform, move |p| {
        p.patch_friend_meta(&actor, &uid, starred, tags, group)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_stranger_setting(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let allow = offload(&platform, move |p| p.allow_strangers(&actor)).await?;
    Ok(Json(serde_json::json!({
        "allow_strangers": allow
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
    let actor = authed.user_id.clone();
    let allow = body.allow;
    offload(&platform, move |p| p.set_allow_strangers(&actor, allow)).await?;
    Ok(StatusCode::NO_CONTENT)
}
