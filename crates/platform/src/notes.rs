//! **handler 层**（`notes`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::page::{self, listing_response};
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::notes::Note;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/notes", post(create))
        .route("/api/notes", get(list))
        .route("/api/notes/{id}", get(one))
        .route("/api/notes/{id}", put(update))
        .route("/api/notes/{id}/autosave", post(autosave))
        .route("/api/notes/{id}", delete(remove))
}

#[derive(Deserialize)]
pub struct CreateBody {
    /// 客户端生成的 id：重复提交同一 id 返回已存在的笔记，不覆盖内容。
    #[serde(default)]
    pub note_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub expected_version: i64,
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn create(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<CreateBody>,
) -> std::result::Result<(StatusCode, Json<Note>), ApiError> {
    // ADR-28：写笔记 = 主行 + 版本行一个事务，整段进阻塞线程池。
    let CreateBody {
        note_id,
        title,
        body: text,
    } = body;
    let user = authed.user_id.clone();
    let note = offload(&platform, move |p| {
        p.create_note(&user, note_id.as_deref(), &title, &text)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(note)))
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let cursor = q.cursor.clone();
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    let (items, next, has_more) =
        offload(&platform, move |p| p.notes(&user, cursor.as_deref(), limit)).await?;
    Ok(listing_response(items, next, has_more))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<Note>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(offload(&platform, move |p| p.note(&user, &id)).await?))
}

async fn update(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> std::result::Result<Json<Note>, ApiError> {
    let user = authed.user_id.clone();
    let UpdateBody {
        title,
        body: text,
        expected_version,
    } = body;
    Ok(Json(
        offload(&platform, move |p| {
            p.update_note(&user, &id, &title, &text, expected_version)
        })
        .await?,
    ))
}

async fn autosave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> std::result::Result<Json<Note>, ApiError> {
    // 自动保存与手工保存走同一条写路径，因此也走同一个 offload：
    // 高频自动保存最容易把 worker 掐死，这里没有特例。
    let user = authed.user_id.clone();
    let UpdateBody {
        title,
        body: text,
        expected_version,
    } = body;
    Ok(Json(
        offload(&platform, move |p| {
            p.update_note(&user, &id, &title, &text, expected_version)
        })
        .await?,
    ))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.delete_note(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
