//! **handler 层**（`stickers`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::stickers::{PackBody, StickerPack};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/stickers/packs", post(create_pack))
        .route("/api/stickers/packs", get(list_packs))
        .route("/api/stickers/packs/{id}", get(one_pack))
        .route("/api/stickers/packs/{id}", delete(remove_pack))
        .route("/api/stickers/packs/{id}/install", post(install))
        .route("/api/stickers/packs/{id}/install", delete(uninstall))
        .route("/api/stickers", get(installed))
        .route("/api/stickers/{id}", put(order))
        .route("/api/stickers/{id}", delete(remove))
}

#[derive(Deserialize)]
pub struct OrderBody {
    pub sort_order: i64,
}

async fn create_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PackBody>,
) -> std::result::Result<(StatusCode, Json<StickerPack>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(platform.create_sticker_pack(&authed.user_id, &body)?),
    ))
}

async fn list_packs(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        serde_json::json!({ "packs": platform.sticker_packs(&authed.user_id)? }),
    ))
}

async fn one_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<StickerPack>, ApiError> {
    Ok(Json(platform.sticker_pack(&authed.user_id, &id)?))
}

async fn remove_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_sticker_pack(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn install(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let added = platform.install_sticker_pack(&authed.user_id, &id)?;
    let pack = platform.sticker_pack(&authed.user_id, &id)?;
    Ok(Json(serde_json::json!({ "added": added, "pack": pack })))
}

async fn uninstall(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let removed = platform.uninstall_sticker_pack(&authed.user_id, &id)?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

async fn installed(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        serde_json::json!({ "stickers": platform.installed_stickers(&authed.user_id)? }),
    ))
}

async fn order(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<OrderBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.order_sticker(&authed.user_id, &id, body.sort_order)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.remove_sticker(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}
