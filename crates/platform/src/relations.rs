//! **handler 层**（`relations`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
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
