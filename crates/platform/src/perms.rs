//! **handler 层**（`perms`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};

use crate::domain::perms::PermsReply;

pub use crate::domain::perms::{parse_perm, Perm};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/communities/{id}/permissions", get(my_perms))
        .route("/api/communities/{id}/members/{user}/role", post(set_role))
        .route("/api/communities/{id}/members/{user}", delete(kick))
        .route("/api/channels/{id}", patch(update_channel))
        .route("/api/channels/{id}", delete(delete_channel))
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
