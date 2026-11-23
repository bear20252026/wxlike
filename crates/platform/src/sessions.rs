//! **handler 层**（`sessions`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
use crate::error::ApiError;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::sessions::{cache_clear_envelope, RefreshReply, STEP_UP_DEVICE_REVOKE};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/sessions", get(list))
        .route("/api/sessions/refresh", post(refresh_session))
        .route("/api/sessions/step-up", post(step_up_grant))
        .route("/api/sessions/{sid}/revoke", post(revoke))
        .route("/api/sessions/revoke-others", post(revoke_others))
        .route("/api/devices/{device_id}/revoke", post(revoke_device))
}

#[derive(Deserialize)]
pub struct RefreshBody {
    pub refresh_token: String,
}

#[derive(Deserialize, Default)]
pub struct StepUpBody {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub totp_code: String,
}

async fn list(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let token = authed.token.clone();
    let (sessions, push) = offload(&platform, move |p| {
        p.touch_session(&token)?;
        let sessions = p.sessions_of(&actor, &token)?;
        let push = p.devices_of(&actor)?;
        Ok((sessions, push))
    })
    .await?;
    Ok(Json(serde_json::json!({
        "sessions": sessions,
        "push_devices": push,
    })))
}

async fn revoke(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(sid): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    offload_api(&platform, move |p| {
        let hit = p.revoke_session_by_sid(&actor, &sid)?;
        if !hit {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "no such session"));
        }
        // 刷新链跟着一起断：只删会话行的话，对方用刷新令牌就能把会话续回来。
        p.revoke_refresh_of_sid(&sid)?;
        p.audit(&actor, "session.revoke", &sid, "");
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_others(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let token = authed.token.clone();
    let n = offload_api(&platform, move |p| {
        let n = p.revoke_other_sessions(&actor, &token)?;
        p.audit(&actor, "session.revoke_others", "", &n.to_string());
        Ok(n)
    })
    .await?;
    Ok(Json(serde_json::json!({ "revoked": n })))
}

/// `POST /api/sessions/refresh`：用刷新令牌换一对新令牌。
///
/// 故意不要求 Bearer——访问令牌通常已经过期，刷新正是为了绕过它。
/// 旧刷新令牌被重放时整条链作废（见 `refresh::rotate`），响应一律 401。
async fn refresh_session(
    State(platform): State<PlatformState>,
    Json(body): Json<RefreshBody>,
) -> std::result::Result<Json<RefreshReply>, ApiError> {
    let refresh_token = body.refresh_token;
    let (rotated, token) =
        offload(&platform, move |p| p.rotate_refresh(&refresh_token, "")).await?;
    Ok(Json(RefreshReply {
        user_id: rotated.user_id,
        sid: rotated.sid,
        device_id: rotated.device_id,
        token,
        refresh_token: rotated.refresh_token,
        expires_at: rotated.expires_at,
    }))
}

/// `POST /api/sessions/step-up`：为敏感操作换一张短时通行证。
///
/// 密码或 TOTP 任一通过即可（复用 `confirm_identity`）；两者都空 → 400，
/// 不把"忘了带凭据"当成验证成功。
async fn step_up_grant(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<StepUpBody>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    if body.password.is_empty() && body.totp_code.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "password or totp_code required",
        ));
    }
    let actor = authed.user_id.clone();
    let token = authed.token.clone();
    let password = body.password;
    let totp_code = body.totp_code;
    let expires_at = offload_api(&platform, move |p| {
        if !p.confirm_identity(&actor, &totp_code, &password)? {
            p.audit(&actor, "session.step_up", "", "failed");
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid credentials",
            ));
        }
        let sid = p.session_sid(&token)?.unwrap_or_default();
        let expires_at = p.grant_step_up(&actor, &sid, STEP_UP_DEVICE_REVOKE)?;
        p.audit(&actor, "session.step_up", "", "ok");
        Ok(expires_at)
    })
    .await?;
    Ok(Json(
        serde_json::json!({ "purpose": STEP_UP_DEVICE_REVOKE, "expires_at": expires_at }),
    ))
}

/// `POST /api/devices/{device_id}/revoke`：下线设备（要求 step-up）。
///
/// 与 `/api/sessions/{sid}/revoke` 的区别：这里按**设备**撤销，同时作废该设备
/// 的刷新链——否则对方拿刷新令牌就能把会话续回来。
async fn revoke_device(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(device_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let token = authed.token.clone();
    let device_id_for_task = device_id.clone();
    let revoked = offload_api(&platform, move |p| {
        let sid = p.session_sid(&token)?.unwrap_or_default();
        p.consume_step_up(&actor, &sid, STEP_UP_DEVICE_REVOKE)?;
        let revoked = p.revoke_device_sessions(&actor, &device_id_for_task)?;
        p.audit(&actor, "device.revoke", &device_id_for_task, "");
        Ok(revoked)
    })
    .await?;
    platform
        .emit(cache_clear_envelope(&authed.user_id, &device_id))
        .await;
    Ok(Json(
        serde_json::json!({ "revoked": revoked, "device_id": device_id }),
    ))
}
