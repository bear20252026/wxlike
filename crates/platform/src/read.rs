//! **handler 层**（`read`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::{ApiError, PlatformError};
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::read::{SyncView, UnreadView};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/conv/{conv}/unread", get(unread))
        .route("/api/conv/{conv}/read", post(mark_read))
        .route("/api/conv/{conv}/sync", get(sync))
}

#[derive(Deserialize, Default)]
pub struct MarkRead {
    /// 读到第几条；缺省 = 读到最新。
    #[serde(default)]
    pub seq: Option<u64>,
}

async fn unread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<UnreadView>, ApiError> {
    let actor = authed.user_id.clone();
    let view = offload(&platform, move |p| p.conv_unread(&actor, &conv)).await?;
    Ok(Json(view))
}

async fn mark_read(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    body: axum::body::Bytes,
) -> std::result::Result<(StatusCode, Json<UnreadView>), ApiError> {
    // 空 body 合法（读到最新）；`Option<Json<T>>` 接不住空 body。
    let upto = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<MarkRead>(&body)
            .map_err(|_| PlatformError::BadInput("bad read body"))?
            .seq
    };
    let actor = authed.user_id.clone();
    let view = offload(&platform, move |p| p.mark_conv_read(&actor, &conv, upto)).await?;
    Ok((StatusCode::OK, Json(view)))
}

#[derive(Deserialize)]
struct SyncQuery {
    #[serde(default)]
    version: u64,
}

async fn sync(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Query(q): Query<SyncQuery>,
) -> std::result::Result<Json<SyncView>, ApiError> {
    let actor = authed.user_id.clone();
    let view = offload(&platform, move |p| p.conv_sync(&actor, &conv, q.version)).await?;
    Ok(Json(view))
}
