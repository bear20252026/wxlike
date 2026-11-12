//! **handler 层**（`call`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::validate;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use wxlike_rtc::room::{EgressPlan, Track};

use crate::domain::call::{require_body_target, TargetBody, DEFAULT_CAPACITY};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/call/{conv}", get(state))
        .route("/api/call/{conv}/start", post(start))
        .route("/api/call/{conv}/join", post(join))
        .route("/api/call/{conv}/leave", post(leave))
        .route("/api/call/{conv}/mute", post(mute))
        .route("/api/call/{conv}/mute-all", post(mute_all))
        .route("/api/call/{conv}/kick", post(kick))
        .route("/api/call/{conv}/lock", post(lock))
        .route("/api/call/{conv}/record", post(record))
        .route("/api/call/{conv}/hand", post(hand))
        .route("/api/call/{conv}/transfer", post(transfer))
        .route("/api/call/{conv}/egress", get(egress))
        .route("/api/call/{conv}/signal", post(post_signal))
        .route("/api/call/{conv}/signal", get(take_signals))
}

#[derive(serde::Deserialize)]
pub struct StartBody {
    #[serde(default)]
    pub capacity: Option<usize>,
}

#[derive(serde::Deserialize)]
pub struct MuteBody {
    /// `mute` 必填；`mute-all` 不需要（全员）。
    #[serde(default)]
    pub target: Option<String>,
    /// "audio" | "video"
    pub track: String,
    pub muted: bool,
}

#[derive(serde::Deserialize)]
pub struct ToggleBody {
    pub value: bool,
}

#[derive(serde::Deserialize, Default)]
pub struct EgressQuery {
    #[serde(default)]
    pub estimate_bps: Option<u64>,
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    match platform.call_state(&conv, &authed.user_id)? {
        Some(view) => Ok(Json(serde_json::json!({
            "conv_id": view.conv_id,
            "live": view.live,
            "host": view.host,
            "locked": view.locked,
            "recording": view.recording,
            "participants": view.participants,
        }))),
        None => Ok(Json(serde_json::json!({
            "conv_id": conv, "live": false, "participants": []
        }))),
    }
}

async fn start(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    // 空 body 也合法（客户端只想用默认容量）。这里刻意不用 `Json<T>`：
    // 它会把空 body 直接判成 400，而"开房不带参数"是正常用法。
    body: axum::body::Bytes,
) -> std::result::Result<StatusCode, ApiError> {
    let capacity = if body.is_empty() {
        DEFAULT_CAPACITY
    } else {
        serde_json::from_slice::<StartBody>(&body)
            .ok()
            .and_then(|b| b.capacity)
            .unwrap_or(DEFAULT_CAPACITY)
    };
    platform.start_call(&conv, &authed.user_id, capacity)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn join(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.join_call(&conv, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.leave_call(&conv, &authed.user_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mute(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<MuteBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let raw = body
        .target
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "target"))?;
    let target = validate::id(raw)?.to_string();
    let track = match body.track.as_str() {
        "audio" => Track::Audio,
        "video" => Track::Video,
        _ => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "track")),
    };
    platform.mute_in_call(&conv, &authed.user_id, &target, track, body.muted)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mute_all(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<MuteBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let track = match body.track.as_str() {
        "audio" => Track::Audio,
        "video" => Track::Video,
        _ => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "track")),
    };
    let affected = platform.mute_all_in_call(&conv, &authed.user_id, track, body.muted)?;
    Ok(Json(serde_json::json!({ "affected": affected })))
}

async fn kick(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<TargetBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let target = require_body_target(&body)?;
    platform.kick_from_call(&conv, &authed.user_id, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn lock(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<ToggleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.lock_call(&conv, &authed.user_id, body.value)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn record(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<ToggleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.record_call(&conv, &authed.user_id, body.value)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn hand(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<ToggleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.hand_in_call(&conv, &authed.user_id, body.value)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn transfer(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<TargetBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let target = require_body_target(&body)?;
    platform.transfer_call(&conv, &authed.user_id, &target)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_signal(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(signal): ApiJson<wxlike_rtc::signal::Signal>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let dropped = platform.post_call_signal(&conv, &authed.user_id, signal)?;
    Ok(Json(serde_json::json!({ "dropped": dropped })))
}

async fn take_signals(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let signals = platform.take_call_signals(&conv, &authed.user_id)?;
    Ok(Json(serde_json::json!({ "signals": signals })))
}

async fn egress(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    axum::extract::Query(q): axum::extract::Query<EgressQuery>,
) -> std::result::Result<Json<EgressPlan>, ApiError> {
    let plan = platform.call_egress(
        &conv,
        &authed.user_id,
        q.estimate_bps
            .unwrap_or(wxlike_rtc::sfu::DEFAULT_MAX_EGRESS_BPS),
    )?;
    Ok(Json(plan))
}
