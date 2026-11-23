//! **handler 层**（`video_refs`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::db::{offload, offload_api};
use crate::error::{ApiError, PlatformError};
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::domain::video_refs::{explain, store_callback, CallbackBody, VideoRefView};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/video/callback", post(callback))
        .route("/api/video/ref/{video_id}", get(get_ref))
}

async fn callback(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<CallbackBody>,
) -> std::result::Result<Json<VideoRefView>, ApiError> {
    let view = offload_api(&platform, move |p| {
        store_callback(p, &headers, body).map_err(explain)
    })
    .await?;
    Ok(Json(view))
}

async fn get_ref(
    State(platform): State<PlatformState>,
    Path(video_id): Path<String>,
) -> std::result::Result<Json<VideoRefView>, ApiError> {
    let view = offload(&platform, move |p| p.video_ref(&video_id)).await?;
    view.map(Json)
        .ok_or_else(|| ApiError::from(PlatformError::NotFound))
}
