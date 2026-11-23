//! **handler 层**（`notification`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route(
            "/api/notification-rules/{conv_id}",
            put(set_notification_rule),
        )
        .route(
            "/api/notification-rules/{conv_id}",
            get(get_notification_rule),
        )
        .route("/api/notification-rules", get(list_notification_rules))
}

#[derive(Deserialize)]
struct RuleBody {
    level: String,
    #[serde(default)]
    keywords: Vec<String>,
}

async fn set_notification_rule(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<RuleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let level = body.level;
    let keywords = body.keywords;
    offload(&platform, move |p| {
        p.set_notification_rule(&actor, &conv, &level, &keywords)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_notification_rule(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rule = offload(&platform, move |p| p.notification_rule(&actor, &conv)).await?;
    Ok(Json(serde_json::json!({ "rule": rule })))
}

async fn list_notification_rules(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let rules = offload(&platform, move |p| p.notification_rules(&actor)).await?;
    Ok(Json(serde_json::json!({ "rules": rules })))
}
