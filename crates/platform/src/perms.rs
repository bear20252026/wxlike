//! **handler 层**（`perms`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
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
    let actor = authed.user_id.clone();
    let id_for_task = id.clone();
    let perms = offload(&platform, move |p| p.effective_perms(&id_for_task, &actor)).await?;
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
    let actor = authed.user_id.clone();
    let role = body.role;
    offload(&platform, move |p| {
        p.set_member_role(&id, &actor, &user, &role)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn kick(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((id, user)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.kick_member(&id, &actor, &user)).await?;
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
    let actor = authed.user_id.clone();
    let name = body.name;
    let kind = body.kind;
    offload(&platform, move |p| {
        p.update_channel(&id, &actor, name.as_deref(), kind.as_deref())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_channel(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.delete_channel(&id, &actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}
