//! **handler 层**（`moments`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
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

use crate::domain::moments::{default_visibility, require_visible, MomentDetail, Visibility};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/moments", post(publish))
        .route("/api/moments/feed", get(feed))
        .route("/api/moments/album", get(album))
        .route("/api/moments/drafts", post(save_draft))
        .route("/api/moments/drafts", get(list_drafts))
        .route("/api/moments/drafts/{draft_id}", delete(remove_draft))
        .route("/api/moments/{moment_id}", get(detail))
        .route("/api/moments/{moment_id}", delete(remove))
        .route("/api/moments/{moment_id}/likes", post(like))
        .route("/api/moments/{moment_id}/likes", delete(unlike))
        .route("/api/moments/{moment_id}/comments", post(comment))
        .route("/api/moments/{moment_id}/report", post(report))
        .route(
            "/api/moments/{moment_id}/comments/{comment_id}",
            delete(remove_comment),
        )
}

#[derive(Deserialize)]
pub struct PublishBody {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
    /// 分组可见时的作者好友分组名。
    #[serde(default)]
    pub audience_group: String,
    /// 从这个草稿发布；成功后草稿删除。不属于调用者则拒绝。
    #[serde(default)]
    pub draft_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CommentBody {
    pub text: String,
    #[serde(default)]
    pub reply_to_comment_id: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct FeedQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct DraftBody {
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
    #[serde(default)]
    pub audience_group: String,
}

async fn publish(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PublishBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // ADR-28：发布 = 插行 + 写索引 + 附件引用 + 扇出提醒，一个事务，
    // 整段搬到阻塞线程池；handler 只 await，不拿连接。
    let PublishBody {
        text,
        media_ids,
        visibility,
        audience_group,
        draft_id,
    } = body;
    let author = authed.user_id.clone();
    let id = offload(&platform, move |p| {
        p.publish_moment_with(
            &author,
            &text,
            &media_ids,
            visibility,
            &audience_group,
            draft_id.as_deref(),
        )
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "moment_id": id })),
    ))
}

async fn save_draft(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<DraftBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let DraftBody {
        draft_id,
        text,
        media_ids,
        visibility,
        audience_group,
    } = body;
    let author = authed.user_id.clone();
    let id = offload(&platform, move |p| {
        p.save_moment_draft(
            &author,
            draft_id.as_deref(),
            &text,
            &media_ids,
            visibility,
            &audience_group,
        )
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "draft_id": id })),
    ))
}

async fn list_drafts(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let author = authed.user_id.clone();
    let drafts = offload(&platform, move |p| p.moment_drafts(&author)).await?;
    Ok(Json(serde_json::json!({ "drafts": drafts })))
}

async fn remove_draft(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(draft_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let draft_id = validate::id(&draft_id).map_err(ApiError::from)?.to_string();
    let author = authed.user_id.clone();
    offload(&platform, move |p| {
        p.delete_moment_draft(&author, &draft_id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ReportBody {
    pub reason: String,
    #[serde(default)]
    pub detail: String,
}

async fn report(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
    ApiJson(body): ApiJson<ReportBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let reason = wxlike_report::Reason::parse(&body.reason)
        .ok_or_else(|| ApiError::from(PlatformError::BadInput("unknown report reason")))?;
    let viewer = authed.user_id.clone();
    let detail = body.detail.clone();
    // 举报也先过可见性闸：看不到的动态不能举报，否则举报接口会变成
    // "这条 id 存在吗"的探针；闸门与建单同在一次 offload 里。
    let ticket = offload(&platform, move |p| {
        require_visible(p, &moment_id, &viewer)?;
        p.create_report(&viewer, "moment", &moment_id, reason, &detail)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "report_id": ticket.id })),
    ))
}

async fn detail(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<Json<MomentDetail>, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let viewer = authed.user_id.clone();
    let detail = offload(&platform, move |p| {
        let (_, head) = p.moment_row(&moment_id)?.ok_or(PlatformError::NotFound)?;
        // 好友可见 / 已被拉黑 / 已删除的动态就是"不存在"，同形返回
        if !p.moment_visible(&viewer, &moment_id)? {
            return Err(PlatformError::NotFound);
        }
        let likes = p
            .moment_visitors_likes(&moment_id)?
            .into_iter()
            .map(|(id, _name)| id)
            .collect();
        Ok::<_, PlatformError>(MomentDetail {
            head,
            likes,
            comments: p.moment_comments(&moment_id)?,
        })
    })
    .await?;
    Ok(Json(detail))
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let viewer = authed.user_id.clone();
    // 只有作者能删：`delete_moment` 的 WHERE 里就带 author，所以"不是你的"
    // 与"不存在/已删"一起落到同一个 404 —— 不再用 403 泄露"这个 id 存在"。
    let deleted = offload(&platform, move |p| p.delete_moment(&moment_id, &viewer)).await?;
    if !deleted {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such moment"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn like(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let viewer = authed.user_id.clone();
    let created = offload(&platform, move |p| {
        require_visible(p, &moment_id, &viewer)?;
        p.like_moment(&moment_id, &viewer)
    })
    .await?;
    // idempotent: a repeat like is a 200 that added nothing
    Ok(Json(serde_json::json!({ "liked": true, "added": created })))
}

async fn unlike(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let viewer = authed.user_id.clone();
    let removed = offload(&platform, move |p| {
        require_visible(p, &moment_id, &viewer)?;
        p.unlike_moment(&moment_id, &viewer)
    })
    .await?;
    Ok(Json(
        serde_json::json!({ "liked": false, "removed": removed }),
    ))
}

async fn remove_comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((moment_id, comment_id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let comment_id = validate::id(&comment_id)
        .map_err(ApiError::from)?
        .to_string();
    let viewer = authed.user_id.clone();
    let deleted = offload(&platform, move |p| {
        require_visible(p, &moment_id, &viewer)?;
        p.delete_comment(&moment_id, &comment_id, &viewer)
    })
    .await?;
    if !deleted {
        // 不是评论作者、也不是动态作者；或者已经删过了
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no such comment"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn comment(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(moment_id): Path<String>,
    ApiJson(body): ApiJson<CommentBody>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let moment_id = validate::id(&moment_id)
        .map_err(ApiError::from)?
        .to_string();
    let viewer = authed.user_id.clone();
    let text = body.text.clone();
    let reply_to = body.reply_to_comment_id.clone();
    let id = offload(&platform, move |p| {
        require_visible(p, &moment_id, &viewer)?;
        p.comment_moment(&moment_id, &viewer, &text, reply_to.as_deref())
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "comment_id": id })),
    ))
}

async fn feed(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<FeedQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let viewer = authed.user_id.clone();
    let (rows, next, has_more) = offload(&platform, move |p| {
        p.feed(
            &viewer,
            q.since.filter(|s| *s > 0),
            q.limit.unwrap_or(page::DEFAULT_PAGE),
        )
    })
    .await?;
    Ok(Json(serde_json::json!({
        "moments": rows,
        "next_cursor": next,
        "has_more": has_more,
    })))
}

#[derive(Deserialize)]
struct AlbumQuery {
    /// 相册主人的用户 id。
    user_id: String,
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

/// 个人主页相册。见 [`Platform::moment_album`]：任何"看不见"的情形都返回
/// 空页，与"没有内容"同形。
async fn album(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<AlbumQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let owner = q.user_id.clone();
    let viewer = authed.user_id.clone();
    let (rows, next, has_more) = offload(&platform, move |p| {
        p.moment_album(
            &viewer,
            &owner,
            q.since.filter(|s| *s > 0),
            q.limit.unwrap_or(page::DEFAULT_PAGE),
        )
    })
    .await?;
    Ok(Json(serde_json::json!({
        "moments": rows,
        "next_cursor": next,
        "has_more": has_more,
    })))
}
