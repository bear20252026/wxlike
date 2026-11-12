//! **handler 层**（`video_feed`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::{AdminAuth, Authed};
use crate::db::offload;
use crate::error::{ApiError, PlatformError};
use crate::extract::ApiJson;
use crate::page;
use crate::validate;
use crate::PlatformState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::video_feed::{default_mode, ReviewResult, VideoView};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/videos", post(publish))
        .route("/api/videos/feed", get(feed))
        .route("/api/videos/{id}", get(one))
        .route("/api/videos/{id}", delete(remove))
        .route("/api/videos/{id}/like", post(like))
        .route("/api/videos/{id}/like", delete(unlike))
        .route("/api/videos/{id}/comments", get(comments))
        .route("/api/videos/{id}/comments", post(comment))
        .route(
            "/api/videos/{id}/comments/{comment_id}",
            delete(remove_comment),
        )
        .route("/api/videos/{id}/report", post(report))
        .route("/api/videos/{id}/review", post(review))
        .route("/api/video-creators/{id}/follow", post(follow))
        .route("/api/video-creators/{id}/follow", delete(unfollow))
}

#[derive(Deserialize)]
pub struct PublishVideoBody {
    pub media_id: String,
    #[serde(default)]
    pub caption: String,
}

#[derive(Deserialize)]
pub struct CommentBody {
    pub text: String,
}

async fn publish(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PublishVideoBody>,
) -> std::result::Result<(StatusCode, Json<VideoView>), ApiError> {
    let author = authed.user_id.clone();
    let media_id = body.media_id.clone();
    let caption = body.caption.clone();
    Ok((
        StatusCode::CREATED,
        Json(
            offload(&platform, move |p| {
                p.publish_video(&author, &media_id, &caption)
            })
            .await?,
        ),
    ))
}

#[derive(Deserialize)]
struct FeedQuery {
    #[serde(default = "default_mode")]
    mode: String,
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn feed(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<FeedQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (items, next, has_more) = platform.video_feed(
        &authed.user_id,
        &q.mode,
        q.cursor.as_deref(),
        q.limit.unwrap_or(page::DEFAULT_PAGE),
    )?;
    Ok(page::listing_response(items, next, has_more))
}

async fn one(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<VideoView>, ApiError> {
    let user = authed.user_id.clone();
    Ok(Json(
        offload(&platform, move |p| p.video(&user, &id)).await?,
    ))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.delete_video(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn like(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let (added, count) = offload(&platform, move |p| p.like_video(&id, &user)).await?;
    Ok(Json(
        serde_json::json!({ "liked": true, "added": added, "count": count }),
    ))
}

async fn unlike(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let (removed, count) = offload(&platform, move |p| p.unlike_video(&id, &user)).await?;
    Ok(Json(
        serde_json::json!({ "liked": false, "removed": removed, "count": count }),
    ))
}

#[derive(Deserialize)]
struct CommentQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
}

async fn comments(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    Query(q): Query<CommentQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let user = authed.user_id.clone();
    let cursor = q.cursor;
    let limit = q.limit.unwrap_or(page::DEFAULT_PAGE);
    // 授权与取页在同一次 offload 里：分成两次会把"可见性判定"和"读评论"
    // 之间留出 await 间隙，删帖/拉黑正好能挤进来。
    let items = offload(&platform, move |p| {
        if !p.video_visible(&user, &id)? {
            return Err(PlatformError::NotFound);
        }
        p.video_comments(&id, cursor, limit)
    })
    .await?;
    let next = items.last().map(|c| c.seq).unwrap_or(0);
    Ok(Json(
        serde_json::json!({ "items": items, "next_cursor": next }),
    ))
}

async fn comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<CommentBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let user = authed.user_id.clone();
    let text = body.text.clone();
    let id = offload(&platform, move |p| p.comment_video(&id, &user, &text)).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "comment_id": id })),
    ))
}

async fn remove_comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((video_id, comment_id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| {
        p.delete_video_comment(&video_id, &comment_id, &user)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ReportBody {
    reason: String,
    #[serde(default)]
    detail: String,
}

async fn report(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReportBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let id = validate::id(&id).map_err(ApiError::from)?.to_string();
    let reason = wxlike_report::Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::from(PlatformError::BadInput("unknown report reason")))?;
    let user = authed.user_id.clone();
    let detail = body.detail.clone();
    let ticket = offload(&platform, move |p| {
        if !p.video_visible(&user, &id)? {
            return Err(PlatformError::NotFound);
        }
        p.create_report(&user, "video", &id, reason, &detail)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "report_id": ticket.id })),
    ))
}

#[derive(Deserialize)]
struct ReviewBody {
    /// `publish` = 放行，`reject` = 驳回。
    pub action: String,
}

/// 运营复核待审视频。`AdminAuth` 排在 body 之前：没令牌就是 403，
/// 不会先解析 body 而把动作枚举暴露出去。
async fn review(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<ReviewBody>,
) -> std::result::Result<Json<ReviewResult>, ApiError> {
    let actor = admin.actor.clone();
    let action = body.action.clone();
    let result = offload(&platform, move |p| p.review_video(&id, &actor, &action)).await?;
    Ok(Json(result))
}

async fn follow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.follow_creator(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unfollow(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let user = authed.user_id.clone();
    offload(&platform, move |p| p.unfollow_creator(&user, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
