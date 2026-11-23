//! **handler 层**（`users`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::db::{offload, offload_api};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::validate;
use crate::PlatformState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::domain::users::{
    auth_reply, client_key, AuthReply, Credentials, PeerAddr, Profile, ProfilePatch, PublicProfile,
};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/2fa/enable", post(totp_enable))
        .route("/api/2fa/confirm", post(totp_confirm))
        .route("/api/2fa/disable", post(totp_disable))
        .route("/api/me", get(me))
        .route("/api/profile", put(update_profile))
        .route("/api/profile/{user_id}", get(public_profile))
        .route("/api/logout", post(logout))
}

async fn register(
    State(platform): State<PlatformState>,
    PeerAddr(peer): PeerAddr,
    headers: axum::http::HeaderMap,
    ApiJson(req): ApiJson<Credentials>,
) -> std::result::Result<(StatusCode, Json<AuthReply>), ApiError> {
    // P4 风控：注册按来源限流；命中挑战则必须过人机校验（428 让客户端取题）。
    // 来源默认取真实 peer（issue #19），没有时按账号名计数。
    let client = client_key(peer, &headers, platform.options().trust_proxy);
    let register_key = client
        .clone()
        .unwrap_or_else(|| format!("name:{}", req.name));
    let captcha = req
        .captcha()
        .map(|(id, answer)| (id.to_string(), answer.to_string()));
    let name = req.name;
    let password = req.password;
    let device_id = req.device_id;
    let device_name = req.device_name;
    let client_for_task = client.clone();
    let session_ttl = platform.options().session_ttl_ms;
    let reply = offload_api(&platform, move |p| {
        p.risk_gate(
            wxlike_risk::Action::Register,
            &register_key,
            captcha
                .as_ref()
                .map(|(id, answer)| (id.as_str(), answer.as_str())),
        )?;
        let user_id = p.register(&name, &password)?;
        p.risk_record(wxlike_risk::Action::Register, &register_key)?;
        let token = p.create_session_for_device(
            &user_id,
            session_ttl,
            &device_id,
            &device_name,
            client_for_task.as_deref().unwrap_or(""),
        )?;
        Ok(auth_reply(p, user_id, token, ""))
    })
    .await?;
    Ok((StatusCode::OK, Json(reply)))
}

/// 重新 enroll 时要求带上**当前第二因子或密码**（issue #5）。
///
/// 两者都可空：首次 enroll（还没启用）不需要任何凭据——那时候也没什么可保护
/// 的；但**已启用**之后必须证明是本人，否则持有 Bearer 的人可以直接把
/// `totp_enabled` 清成 0，二次认证就没了。
#[derive(Deserialize, Default)]
pub struct TotpEnroll {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub password: String,
}

async fn totp_enable(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<TotpEnroll>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let actor = authed.user_id.clone();
    let code = body.code;
    let password = body.password;
    let secret = offload_api(&platform, move |p| {
        if p.totp_enabled(&actor)? {
            if !p.confirm_identity(&actor, &code, &password)? {
                p.audit(&actor, "user.2fa.re_enroll", &actor, "denied");
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "re-enrolling 2FA requires the current code or password",
                ));
            }
            p.audit(&actor, "user.2fa.re_enroll", &actor, "ok");
        }
        p.totp_enable(&actor).map_err(ApiError::from)
    })
    .await?;
    Ok(Json(serde_json::json!({ "secret": secret })))
}

#[derive(Deserialize)]
struct TotpCode {
    code: String,
}

async fn totp_confirm(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<TotpCode>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let code = body.code;
    offload(&platform, move |p| p.totp_confirm(&actor, &code)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn totp_disable(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<TotpCode>,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let code = body.code;
    offload(&platform, move |p| p.totp_disable(&actor, &code)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn login(
    State(platform): State<PlatformState>,
    PeerAddr(peer): PeerAddr,
    headers: axum::http::HeaderMap,
    ApiJson(req): ApiJson<Credentials>,
) -> std::result::Result<(StatusCode, Json<AuthReply>), ApiError> {
    // 失败计数按账号 + 来源双键：只按账号会被"换号撞库"绕过，只按来源会
    // 让同一 NAT 后的正常用户互相连坐——两个键都记，取更严的那个。
    let account_key = format!("login:{}", req.name);
    let client = client_key(peer, &headers, platform.options().trust_proxy);
    let captcha = req
        .captcha()
        .map(|(id, answer)| (id.to_string(), answer.to_string()));
    let name = req.name;
    let password = req.password;
    let totp_code = req.totp_code;
    let device_id = req.device_id;
    let device_name = req.device_name;
    let session_ttl = platform.options().session_ttl_ms;
    let reply = offload_api(&platform, move |p| {
        let mut checks = vec![(wxlike_risk::Action::Login, account_key.as_str())];
        if let Some(c) = client.as_deref() {
            checks.push((wxlike_risk::Action::Login, c));
        }
        p.risk_gate_many(
            &checks,
            captcha
                .as_ref()
                .map(|(id, answer)| (id.as_str(), answer.as_str())),
        )?;

        let verified = p.verify_credentials(&name, &password)?;
        let Some(user_id) = verified else {
            p.risk_record(wxlike_risk::Action::Login, &account_key)?;
            if let Some(c) = client.as_deref() {
                p.risk_record(wxlike_risk::Action::Login, c)?;
            }
            p.audit(&name, "user.login", "", "failed");
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid credentials",
            ));
        };
        // **全部因子通过之后才清空失败计数**（issue #6）。
        //
        // 原来这里先 `risk_clear` 再校验 TOTP：知道密码的人可以拿错误的 6 位码
        // 无限尝试，每次失败都把计数清零——等于 TOTP 没有失败限流。
        if let Err(err) = p.totp_check_login(&user_id, totp_code.as_deref()) {
            p.risk_record(wxlike_risk::Action::Login, &account_key)?;
            if let Some(c) = client.as_deref() {
                p.risk_record(wxlike_risk::Action::Login, c)?;
            }
            p.audit(&name, "user.login", "", "totp_failed");
            return Err(err.into());
        }
        p.risk_clear(wxlike_risk::Action::Login, &account_key)?;
        if let Some(c) = client.as_deref() {
            p.risk_clear(wxlike_risk::Action::Login, c)?;
        }
        let token = p.create_session_for_device(
            &user_id,
            session_ttl,
            &device_id,
            &device_name,
            client.as_deref().unwrap_or(""),
        )?;
        p.audit(&user_id, "user.login", &user_id, "ok");
        Ok(auth_reply(p, user_id, token, &device_id))
    })
    .await?;
    Ok((StatusCode::OK, Json(reply)))
}

async fn me(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Profile>, ApiError> {
    let actor = authed.user_id.clone();
    let profile = offload(&platform, move |p| p.profile_of(&actor)).await?;
    Ok(Json(profile))
}

async fn update_profile(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(patch): ApiJson<ProfilePatch>,
) -> std::result::Result<Json<Profile>, ApiError> {
    let changed = patch.changed_fields();
    if changed.is_empty() {
        // 三个字段都没给：更可能是客户端漏了字段，而不是"故意什么都不改"。
        // 回一个 200 + 原样资料会把这次调用记成一次成功的 profile.update。
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "empty profile patch",
        ));
    }
    let actor = authed.user_id.clone();
    let profile = offload(&platform, move |p| {
        let profile = p.update_profile(&actor, &patch)?;
        // 审计记**改了哪些字段**，不记改成什么：昵称/签名/头像路径都是个人数据。
        p.audit(&actor, "profile.update", "profile", &changed.join(","));
        Ok(profile)
    })
    .await?;
    Ok(Json(profile))
}

async fn public_profile(
    State(platform): State<PlatformState>,
    Path(user_id): Path<String>,
) -> std::result::Result<Json<PublicProfile>, ApiError> {
    let user_id = validate::id(&user_id)?.to_string();
    let p = offload(&platform, move |p| p.profile_of(&user_id))
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "no such user"))?;
    Ok(Json(PublicProfile {
        user_id: p.user_id,
        name: p.name,
        avatar: p.avatar,
    }))
}

async fn logout(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<StatusCode, ApiError> {
    let actor = authed.user_id.clone();
    let token = authed.token;
    offload_api(&platform, move |p| {
        p.revoke_session(&token)?;
        p.audit(&actor, "user.logout", "", "");
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
