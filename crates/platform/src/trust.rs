//! **handler 层**（`trust`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::{AdminAuth, Authed};
use crate::db::{offload, offload_api};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use wxlike_moderation::{Decision, Mode};

use crate::domain::trust::QueuedItem;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/moderation/check", post(check))
        .route("/api/admin/moderation/queue", get(queue))
        .route("/api/admin/moderation/{id}/release", post(release))
        .route("/api/admin/moderation/{id}/reject", post(reject))
        .route("/api/admin/moderation/mode", put(set_mode))
}

#[derive(serde::Deserialize)]
struct CheckBody {
    text: String,
}

async fn check(
    State(platform): State<PlatformState>,
    _authed: Authed,
    ApiJson(body): ApiJson<CheckBody>,
) -> std::result::Result<Json<Decision>, ApiError> {
    let text = body.text;
    let decision = offload(&platform, move |p| Ok(p.moderate(&text))).await?;
    Ok(Json(decision))
}

async fn queue(
    State(platform): State<PlatformState>,
    _admin: AdminAuth,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (mode, queue) = offload(&platform, |p| {
        let mode = p.moderation_mode();
        let queue = p.moderation_queue(100)?;
        Ok((mode, queue))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "mode": mode,
        "queue": queue,
    })))
}

async fn release(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
) -> std::result::Result<Json<QueuedItem>, ApiError> {
    let actor = admin.actor;
    let item = offload(&platform, move |p| p.release_moderated(&id, &actor)).await?;
    match item {
        Some(item) => Ok(Json(item)),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "no such queued item")),
    }
}

async fn reject(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = admin.actor;
    let ok = offload(&platform, move |p| p.reject_moderated(&id, &actor)).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such queued item"))
    }
}

#[derive(serde::Deserialize)]
struct ModeBody {
    mode: Mode,
}

async fn set_mode(
    State(platform): State<PlatformState>,
    admin: AdminAuth,
    ApiJson(body): ApiJson<ModeBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = admin.actor;
    let mode = body.mode;
    offload_api(&platform, move |p| {
        p.set_moderation_mode(mode);
        p.audit(
            &actor,
            "moderation.mode",
            "",
            match mode {
                Mode::PreReview => "pre_review",
                Mode::PostReview => "post_review",
            },
        );
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
