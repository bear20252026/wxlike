//! **handler 层**（`recovery`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::PlatformState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};

use crate::domain::recovery::{ResetBody, CAPTCHA_TTL_MS, RECOVERY_TTL_MS};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/captcha", post(issue_captcha))
        .route("/api/recovery/code", post(issue_recovery))
        .route("/api/recovery/reset", post(reset))
}

async fn issue_captcha(
    State(platform): State<PlatformState>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let (id, question) = platform.issue_captcha()?;
    Ok(Json(serde_json::json!({
        "challenge_id": id,
        "question": question,
        "expires_in_ms": CAPTCHA_TTL_MS,
    })))
}

async fn issue_recovery(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // 二次确认因子：2FA 已启用时必须带验证码，否则任何拿到 token 的人都能
    // 生成恢复码把账号永久接管。
    if platform.totp_enabled(&authed.user_id)? {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "totp code required to issue a recovery code",
        ));
    }
    let code = platform.issue_recovery_code(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "recovery_code": code,
        "expires_in_ms": RECOVERY_TTL_MS,
    })))
}

async fn reset(
    State(platform): State<PlatformState>,
    ApiJson(body): ApiJson<ResetBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let key = format!("recovery:{}", body.name);
    platform.risk_gate(wxlike_risk::Action::Login, &key, body.captcha())?;
    // **失败要计数、成功要清空**（issue #19）。原来是反的：`?` 让失败直接返回
    // 而不记录，成功却调了 `risk_record`——于是无效恢复码可以无限猜，
    // 而真的重置成功反而给这个账号攒了一次"失败"。
    match platform.reset_password_with_code(&body.name, &body.recovery_code, &body.new_password) {
        Ok(_) => {
            platform.risk_clear(wxlike_risk::Action::Login, &key)?;
            Ok(StatusCode::NO_CONTENT)
        }
        Err(err) => {
            platform.risk_record(wxlike_risk::Action::Login, &key)?;
            Err(err.into())
        }
    }
}
