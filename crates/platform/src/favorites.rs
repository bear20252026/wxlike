//! **handler 层**（`favorites`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::page;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::favorites::{Favorite, FavoriteInput};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/favorites", post(create))
        .route("/api/favorites", get(list))
        .route("/api/favorites/objects", post(create_object))
        .route("/api/favorites/search", get(search))
        .route("/api/favorites/reorder", post(reorder))
        .route("/api/favorites/{id}", get(one))
        .route("/api/favorites/{id}", put(update))
        .route("/api/favorites/{id}", delete(remove))
}

/// 收藏一个内容对象（限制引用 + 服务端快照）。
#[derive(Deserialize)]
pub struct ObjectFavoriteBody {
    pub kind: String,
    pub id: String,
}

#[derive(Deserialize)]
pub struct FavoriteUpdateBody {
    #[serde(default)]
    pub snapshot_label: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Deserialize)]
pub struct ReorderBody {
    pub ids: Vec<String>,
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(input): ApiJson<FavoriteInput>,
) -> std::result::Result<(StatusCode, Json<Favorite>), ApiError> {
    let user = authed.user_id.clone();
    let favorite = offload(&platform, move |p| p.add_favorite(&user, &input)).await?;
    Ok((StatusCode::CREATED, Json(favorite)))
}

async fn create_object(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ObjectFavoriteBody>,
) -> std::result::Result<(StatusCode, Json<Favorite>), ApiError> {
    let user = authed.user_id.clone();
    let kind = body.kind.clone();
    let id = body.id.clone();
    // 授权在写入前完成，且与写入在同一次 offload 里：不能"先查可见性、
    // 放回 await、再写"，那会在中间窗口里读到过期权限。
    let favorite = offload(&platform, move |p| p.add_object_favorite(&user, &kind, &id)).await?;
    Ok((StatusCode::CREATED, Json(favorite)))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    limit: Option<i64>,
    before_created: Option<i64>,
    cursor: Option<String>,
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    if q.cursor.is_none() && q.before_created.is_some() {
        // 兼容旧客户端：created_at 水位线，仍按时间倒序。
        let user = authed.user_id.clone();
        let before = q.before_created;
        let limit = q.limit.unwrap_or(page::DEFAULT_PAGE) as usize;
        let rows = offload(&platform, move |p| p.list_favorites(&user, before, limit)).await?;
        let next = rows.last().map(|f| f.created_at);
        return Ok(Json(
            serde_json::json!({ "favorites": rows, "next_before_created": next }),
        ));
    }
    let user = authed.user_id.clone();
    let cursor = q.cursor.clone();
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    let (rows, next, has_more) = offload(&platform, move |p| {
        p.favorites_sorted(&user, cursor.as_deref(), limit)
    })
    .await?;
    Ok(Json(serde_json::json!({
        "favorites": rows,
        "next_cursor": next,
        "has_more": has_more,
    })))
}

#[derive(Deserialize)]
struct FavoriteSearchQuery {
    q: String,
    limit: Option<i64>,
}

async fn search(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<FavoriteSearchQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let needle = q.q.clone();
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    let items = offload(&platform, move |p| {
        p.search_favorites(&user, &needle, limit)
    })
    .await?;
    Ok(Json(serde_json::json!({ "items": items })))
}

async fn update(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<FavoriteUpdateBody>,
) -> std::result::Result<Json<Favorite>, ApiError> {
    let user = authed.user_id.clone();
    let label = body.snapshot_label.clone();
    let order = body.sort_order;
    Ok(Json(
        offload(&platform, move |p| {
            p.update_favorite(&user, &id, label.as_deref(), order)
        })
        .await?,
    ))
}

async fn reorder(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ReorderBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let ids = body.ids.clone();
    // 整单校验 + 整单写入是一个事务，也必须是一次 offload：拖拽排序
    // 中途被打断会留下半套 sort_order。
    let updated = offload(&platform, move |p| p.reorder_favorites(&user, &ids)).await?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<Favorite>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| p.favorite(&user, &id)).await?,
    ))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.remove_favorite(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
