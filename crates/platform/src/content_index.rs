//! **handler 层**（`content_index`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::page;
use crate::PlatformState;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::content_index::{
    requested_scopes, DEFAULT_TREND_PAGE, KINDS, MAX_QUERY_CHARS, MAX_SEARCH_PAGE, TREND_WINDOW_MS,
};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/search/content", get(search))
        .route("/api/search/trends", get(trends))
        .route("/api/search/scopes", get(scopes))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    /// 单域过滤（P3 起的既有参数，保留）。
    kind: Option<String>,
    /// 多域过滤，逗号分隔；与 `kind` 同时给出时取并集。
    scope: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn search(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<SearchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let viewer = authed.user_id.clone();
    let input = q.q.clone();
    let scopes = requested_scopes(q.kind.as_deref(), q.scope.as_deref());
    let cursor = q.cursor.clone();
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    // ADR-28：检索是多轮 keyset 查询，整段搬到阻塞线程池，handler 只 await。
    let (items, next_cursor, has_more) = offload(&platform, move |p| {
        p.search_content(&viewer, &input, &scopes, cursor.as_deref(), limit)
    })
    .await?;
    Ok(Json(serde_json::json!({
        "items": items,
        "next_cursor": next_cursor,
        "has_more": has_more,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

#[derive(Deserialize)]
struct TrendQuery {
    limit: Option<usize>,
}

async fn trends(
    State(platform): State<PlatformState>,
    _authed: Authed,
    Query(q): Query<TrendQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_TREND_PAGE);
    let terms = offload(&platform, move |p| p.content_trends(limit)).await?;
    Ok(Json(serde_json::json!({
        "terms": terms,
        "window_ms": TREND_WINDOW_MS,
        "snapshot_version": wxlike_core::types::now_ms(),
    })))
}

/// 自省：这个统一入口现在能搜哪些域、上限是多少。客户端据此渲染 tab，不用猜，
/// 也不用把"支持哪些 scope"硬编码到三端。纯常量读取，不碰数据库。
async fn scopes(_authed: Authed) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "scopes": KINDS,
        "max_page": MAX_SEARCH_PAGE,
        "max_query_chars": MAX_QUERY_CHARS,
        "trend_window_ms": TREND_WINDOW_MS,
    })))
}
