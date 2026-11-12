//! **handler 层**（`inbox`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::page;
use crate::PlatformState;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/notifications", get(list))
        .route("/api/notifications/read", post(mark_read))
}

#[derive(Deserialize)]
struct ListQuery {
    before_seq: Option<i64>,
    limit: Option<i64>,
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (items, next, has_more) = platform.notifications(
        &authed.user_id,
        q.before_seq,
        q.limit.unwrap_or(page::DEFAULT_PAGE),
    )?;
    let unread = platform.unread_notifications(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "items": items,
        "next_cursor": next,
        "has_more": has_more,
        "unread_count": unread,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

#[derive(Deserialize)]
struct ReadBody {
    #[serde(default)]
    up_to_seq: Option<i64>,
    #[serde(default)]
    ids: Vec<String>,
}

async fn mark_read(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ReadBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let changed = platform.mark_notifications_read(&authed.user_id, body.up_to_seq, &body.ids)?;
    let unread = platform.unread_notifications(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "updated": changed,
        "unread_count": unread,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}
