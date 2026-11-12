//! **handler 层**（`status`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::{ApiError, PlatformError};
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::status::{BackgroundDraft, StatusBody, StatusView};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/statuses", post(set_status))
        .route("/api/statuses", get(list_statuses))
        .route("/api/statuses/clear", post(clear_all))
        .route("/api/statuses/background", post(save_background))
        .route("/api/statuses/background", get(background))
        .route("/api/statuses/background", delete(remove_background))
        .route("/api/statuses/{id}", get(one))
        .route("/api/statuses/{id}", delete(clear_one))
        .route("/api/statuses/{id}/react", post(react))
}

#[derive(Deserialize)]
pub struct BackgroundBody {
    pub media_id: String,
}

async fn set_status(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<StatusBody>,
) -> std::result::Result<(StatusCode, Json<StatusView>), ApiError> {
    let user = authed.user_id.clone();
    // 设置状态（含清旧行、消费背景草稿、写索引）与取扇出收件人是同一个
    // 同步临界区，一次 offload 搬走。
    let (view, to) = offload(&platform, move |p| {
        let view = p.set_status(&user, &body)?;
        Ok((view, p.friend_ids(&user)?))
    })
    .await?;
    platform
        .emit(crate::system_envelope(
            &format!("status:{}", authed.user_id),
            &authed.user_id,
            to,
            "StatusChanged",
        ))
        .await;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn list_statuses(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let items = offload(&platform, move |p| p.statuses(&user)).await?;
    Ok(Json(serde_json::json!({
        "items": items,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<StatusView>, ApiError> {
    let user = authed.user_id.clone();
    let view = offload(&platform, move |p| p.status(&user, &id))
        .await?
        .ok_or_else(|| ApiError::from(PlatformError::NotFound))?;
    Ok(Json(view))
}

async fn clear_one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    let cleared_for = user.clone();
    let (to, _) = offload(&platform, move |p| {
        p.clear_status(&cleared_for, &id)?;
        Ok((p.friend_ids(&cleared_for)?, ()))
    })
    .await?;
    platform
        .emit(crate::system_envelope(
            &format!("status:{}", authed.user_id),
            &authed.user_id,
            to,
            "StatusChanged",
        ))
        .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_all(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let all_for = user.clone();
    let (cleared, to) = offload(&platform, move |p| {
        let cleared = p.clear_all_statuses(&all_for)?;
        Ok((cleared, p.friend_ids(&all_for)?))
    })
    .await?;
    platform
        .emit(crate::system_envelope(
            &format!("status:{}", authed.user_id),
            &authed.user_id,
            to,
            "StatusChanged",
        ))
        .await;
    Ok(Json(serde_json::json!({ "cleared": cleared })))
}

async fn react(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let (active, count) = offload(&platform, move |p| p.react_status(&user, &id)).await?;
    Ok(Json(
        serde_json::json!({ "active": active, "count": count }),
    ))
}

async fn save_background(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<BackgroundBody>,
) -> std::result::Result<(StatusCode, Json<BackgroundDraft>), ApiError> {
    let user = authed.user_id.clone();
    let media_id = body.media_id.clone();
    Ok((
        StatusCode::CREATED,
        // 背景草稿先校验归属再落行，一次 offload 内完成
        Json(
            offload(&platform, move |p| {
                p.save_status_background(&user, &media_id)
            })
            .await?,
        ),
    ))
}

async fn background(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(serde_json::json!({
        "background": offload(&platform, move |p| p.status_background(&user)).await?,
    })))
}

async fn remove_background(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.remove_status_background(&user)).await?;
    Ok(StatusCode::NO_CONTENT)
}
