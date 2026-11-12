//! **handler 层**（`articles`）：路由 + 提取器 + 响应组装。
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

use crate::domain::articles::{ArticleBody, ArticleUpdateBody, ArticleView, PublisherView};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/publishers", post(set_publisher))
        .route("/api/publishers", get(list_publishers))
        .route("/api/publishers/{id}", get(one_publisher))
        .route("/api/publishers/{id}/follow", post(follow))
        .route("/api/publishers/{id}/follow", delete(unfollow))
        .route("/api/publishers/{id}/articles", get(publisher_articles))
        .route("/api/articles", post(create))
        .route("/api/articles/{id}", get(one))
        .route("/api/articles/{id}", put(update))
        .route("/api/articles/{id}", delete(remove))
        .route("/api/articles/{id}/read", post(mark_read))
        .route("/api/articles/{id}/react", post(react))
}

#[derive(Deserialize)]
pub struct PublisherBody {
    pub name: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub avatar_media_id: String,
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
}

#[derive(Deserialize)]
struct ArticleListQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct ReactBody {
    pub kind: String,
}

async fn set_publisher(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PublisherBody>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    let user = authed.user_id.clone();
    let name = body.name.clone();
    let bio = body.bio.clone();
    let avatar = body.avatar_media_id.clone();
    Ok(Json(
        offload(&platform, move |p| {
            p.set_publisher(&user, &name, &bio, &avatar)
        })
        .await?,
    ))
}

async fn list_publishers(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let needle = q.q.clone();
    let items = offload(&platform, move |p| p.publishers(&user, needle.as_deref())).await?;
    Ok(Json(
        serde_json::json!({ "items": items, "snapshot_version": wxlike_core::types::now_ms() }),
    ))
}

async fn one_publisher(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| p.publisher_by_id(&id, &user)).await?,
    ))
}

async fn follow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    // 关注 + 回读视图在一次 offload 里：分两次就得把连接所有权穿过 await。
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| {
            p.follow_publisher(&user, &id)?;
            p.publisher_by_id(&id, &user)
        })
        .await?,
    ))
}

async fn unfollow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<PublisherView>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| {
            p.unfollow_publisher(&user, &id)?;
            p.publisher_by_id(&id, &user)
        })
        .await?,
    ))
}

async fn publisher_articles(
    State(platform): State<PlatformState>,
    _authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<ArticleListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let cursor = q.cursor;
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    let (items, next, has_more) = offload(&platform, move |p| {
        p.articles_by_publisher(&id, cursor, limit)
    })
    .await?;
    Ok(page::listing_response(items, next, has_more))
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<ArticleBody>,
) -> std::result::Result<(StatusCode, Json<ArticleView>), ApiError> {
    let author = authed.user_id.clone();
    Ok((
        StatusCode::CREATED,
        // 发布 = 文章行 + 版本行 + 索引 + 附件引用 + 粉丝提醒，一个事务，
        // 整段进阻塞线程池（ADR-28）。
        Json(offload(&platform, move |p| p.create_article(&author, body)).await?),
    ))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<ArticleView>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| p.article_for(&user, &id)).await?,
    ))
}

async fn update(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ArticleUpdateBody>,
) -> std::result::Result<Json<ArticleView>, ApiError> {
    let author = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| p.update_article(&author, &id, body)).await?,
    ))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.delete_article(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_read(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let unread = offload(&platform, move |p| p.mark_article_read(&user, &id)).await?;
    Ok(Json(serde_json::json!({ "unread": unread })))
}

async fn react(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReactBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let kind = body.kind.clone();
    let (active, counts) = offload(&platform, move |p| p.react_article(&user, &id, &kind)).await?;
    Ok(Json(
        serde_json::json!({ "active": active, "counts": counts }),
    ))
}
