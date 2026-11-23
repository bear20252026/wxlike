//! **handler 层**（`stickers`）：路由 + 提取器 + 响应组装。
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
    let actor = authed.user_id.clone();
    let pack = offload(&platform, move |p| p.create_sticker_pack(&actor, &body)).await?;
    Ok((StatusCode::CREATED, Json(pack)))
}

async fn list_packs(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let packs = offload(&platform, move |p| p.sticker_packs(&actor)).await?;
    Ok(Json(serde_json::json!({ "packs": packs })))
}

async fn one_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<StickerPack>, ApiError> {
    let actor = authed.user_id.clone();
    let pack = offload(&platform, move |p| p.sticker_pack(&actor, &id)).await?;
    Ok(Json(pack))
}

async fn remove_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.delete_sticker_pack(&actor, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn install(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let id_for_task = id.clone();
    let (added, pack) = offload(&platform, move |p| {
        let added = p.install_sticker_pack(&actor, &id_for_task)?;
        let pack = p.sticker_pack(&actor, &id_for_task)?;
        Ok((added, pack))
    })
    .await?;
    Ok(Json(serde_json::json!({ "added": added, "pack": pack })))
}

async fn uninstall(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let removed = offload(&platform, move |p| p.uninstall_sticker_pack(&actor, &id)).await?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

async fn installed(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let stickers = offload(&platform, move |p| p.installed_stickers(&actor)).await?;
    Ok(Json(serde_json::json!({ "stickers": stickers })))
}

async fn order(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<OrderBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let sort_order = body.sort_order;
    offload(&platform, move |p| p.order_sticker(&actor, &id, sort_order)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.remove_sticker(&actor, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
