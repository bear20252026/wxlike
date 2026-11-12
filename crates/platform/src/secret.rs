//! **handler 层**（`secret`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::validate;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};

use crate::domain::secret::{SafetyReply, SecretState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/secret/{conv}", put(configure))
        .route("/api/secret/{conv}", get(state))
        .route("/api/secret/{peer}/safety-number", get(safety_number))
        .route("/api/secret/{peer}/verify", put(verify))
}

#[derive(serde::Deserialize)]
pub struct SecretConfig {
    pub enabled: bool,
    #[serde(default)]
    pub ttl_ms: u64,
}

async fn configure(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    ApiJson(body): ApiJson<SecretConfig>,
) -> std::result::Result<Json<SecretState>, ApiError> {
    let state = platform.set_secret_chat(&authed.user_id, &conv, body.enabled, body.ttl_ms)?;
    platform.audit(
        &authed.user_id,
        "secret.configure",
        &conv,
        if body.enabled { "on" } else { "off" },
    );
    Ok(Json(state))
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<SecretState>, ApiError> {
    // 读也要校验成员：会话状态本身（是否秘密、TTL 多久）属隐私
    let state = platform.secret_state(&conv)?;
    platform
        .require_conv_member(&authed.user_id, &state.conv_id)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "not a participant"))?;
    Ok(Json(state))
}

async fn safety_number(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(peer): Path<String>,
) -> std::result::Result<Json<SafetyReply>, ApiError> {
    let peer = validate::id(&peer)?.to_string();
    let number = platform.safety_number(&authed.user_id, &peer)?;
    let identity = platform
        .public_identity(&peer)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "peer has no published keys"))?;
    Ok(Json(SafetyReply {
        safety_number: number,
        peer_identity: identity,
        verified_at: platform.verification_of(&authed.user_id, &peer)?,
    }))
}

#[derive(serde::Deserialize)]
struct VerifyBody {
    peer_identity: String,
}

async fn verify(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(peer): Path<String>,
    ApiJson(body): ApiJson<VerifyBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let peer = validate::id(&peer)?.to_string();
    platform.verify_peer(&authed.user_id, &peer, &body.peer_identity)?;
    platform.audit(&authed.user_id, "secret.verify", &peer, "");
    Ok(StatusCode::NO_CONTENT)
}
