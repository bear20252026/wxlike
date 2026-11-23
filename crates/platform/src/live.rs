//! **handler 层**（`live`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::offload;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::live::{LiveState, StartReply, INGEST_TTL_MS};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/live/{channel}/start", post(start))
        .route("/api/live/{channel}/stop", post(stop))
        .route("/api/live/{channel}", get(state))
        .route("/api/live/{channel}/join", post(join))
        .route("/api/live/{channel}/leave", post(leave))
        .route("/api/live/{channel}/danmaku", post(danmaku))
        .route("/api/live/ingest/authorize", post(authorize_ingest))
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(default)]
    recording: bool,
}

#[derive(Deserialize)]
struct DanmakuBody {
    text: String,
}

#[derive(Deserialize)]
struct IngestAuthBody {
    stream_key: String,
    token: String,
}

async fn start(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
    ApiJson(body): ApiJson<StartBody>,
) -> std::result::Result<Json<StartReply>, ApiError> {
    let actor = authed.user_id.clone();
    let recording = body.recording;
    let session = offload(&platform, move |p| {
        p.start_live(&channel, &actor, recording)
    })
    .await?;
    let token = platform.ingest_token(&session.stream_key, INGEST_TTL_MS);
    let base = std::env::var("WXLIKE_RTMP_BASE")
        .unwrap_or_else(|_| "rtmp://127.0.0.1:1935/live".to_string());
    let ingest_url = format!("{base}/{}", session.stream_key);
    Ok(Json(StartReply {
        session,
        ingest_token: token,
        ingest_url,
    }))
}

async fn stop(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload(&platform, move |p| p.stop_live(&channel, &actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<Json<LiveState>, ApiError> {
    let actor = authed.user_id.clone();
    let live = offload(&platform, move |p| p.live_state(&channel, &actor)).await?;
    Ok(Json(live))
}

async fn join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let viewers = offload(&platform, move |p| p.join_live(&channel, &actor)).await?;
    Ok(Json(serde_json::json!({ "viewers": viewers })))
}

async fn leave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let viewers = offload(&platform, move |p| p.leave_live(&channel, &actor)).await?;
    Ok(Json(serde_json::json!({ "viewers": viewers })))
}

async fn danmaku(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(channel): Path<String>,
    ApiJson(body): ApiJson<DanmakuBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let delivered = platform
        .send_danmaku(&channel, &authed.user_id, &body.text)
        .await?;
    Ok(Json(serde_json::json!({ "delivered": delivered })))
}

/// RTMP `on_publish` 回调：ingest 节点持共享 secret 调用，无用户 token。
async fn authorize_ingest(
    State(platform): State<PlatformState>,
    ApiJson(body): ApiJson<IngestAuthBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let stream_key = body.stream_key;
    let token = body.token;
    let now = wxlike_core::types::now_ms();
    offload(&platform, move |p| {
        p.verify_ingest(&stream_key, &token, now)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
