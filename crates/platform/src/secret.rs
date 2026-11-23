//! **handler 层**（`secret`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
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
    let actor = authed.user_id.clone();
    let enabled = body.enabled;
    let ttl_ms = body.ttl_ms;
    let state = offload(&platform, move |p| {
        let state = p.set_secret_chat(&actor, &conv, enabled, ttl_ms)?;
        p.audit(
            &actor,
            "secret.configure",
            &conv,
            if enabled { "on" } else { "off" },
        );
        Ok(state)
    })
    .await?;
    Ok(Json(state))
}

async fn state(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<SecretState>, ApiError> {
    let actor = authed.user_id.clone();
    let state = offload_api(&platform, move |p| {
        // 读也要校验成员：会话状态本身（是否秘密、TTL 多久）属隐私
        let state = p.secret_state(&conv)?;
        p.require_conv_member(&actor, &state.conv_id)
            .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "not a participant"))?;
        Ok(state)
    })
    .await?;
    Ok(Json(state))
}

async fn safety_number(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(peer): Path<String>,
) -> std::result::Result<Json<SafetyReply>, ApiError> {
    let peer = validate::id(&peer)?.to_string();
    let actor = authed.user_id.clone();
    let (number, identity, verified_at) = offload_api(&platform, move |p| {
        let number = p.safety_number(&actor, &peer)?;
        let identity = p
            .public_identity(&peer)?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "peer has no published keys"))?;
        let verified_at = p.verification_of(&actor, &peer)?;
        Ok((number, identity, verified_at))
    })
    .await?;
    Ok(Json(SafetyReply {
        safety_number: number,
        peer_identity: identity,
        verified_at,
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
    let actor = authed.user_id.clone();
    let peer_identity = body.peer_identity;
    offload_api(&platform, move |p| {
        p.verify_peer(&actor, &peer, &peer_identity)?;
        p.audit(&actor, "secret.verify", &peer, "");
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
