//! **handler 层**（`inbox`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
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
    let me = authed.user_id.clone();
    let before_seq = q.before_seq;
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    let (items, next, has_more, unread) = offload(&platform, move |p| {
        let (items, next, has_more) = p.notifications(&me, before_seq, limit)?;
        let unread = p.unread_notifications(&me)?;
        Ok((items, next, has_more, unread))
    })
    .await?;
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
    let me = authed.user_id.clone();
    let up_to_seq = body.up_to_seq;
    let ids = body.ids;
    let (changed, unread) = offload(&platform, move |p| {
        let changed = p.mark_notifications_read(&me, up_to_seq, &ids)?;
        let unread = p.unread_notifications(&me)?;
        Ok((changed, unread))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "updated": changed,
        "unread_count": unread,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}
