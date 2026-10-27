//! WP-01 account endpoints: register / login / me / profile / logout.
//!
//! Response shapes are exactly the contract in `docs/WORK_BREAKDOWN.md` WP-01.
//! `GET /api/profile/{user_id}` returns the intentionally narrow public view
//! (`user_id`, `name`, `avatar`); `signature` is self-only and only appears on
//! `/api/me`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::Deserialize;

use crate::auth::{self, Authed};
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

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

#[derive(serde::Deserialize)]
pub struct Credentials {
    pub name: String,
    pub password: String,
    /// P4 2FA：启用 TOTP 后必填。
    #[serde(default)]
    pub totp_code: Option<String>,
    /// P4 设备管理：设备标识与展示名（缺省时按"未知设备"记）。
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub device_name: String,
    /// P4 人机校验：风控要求挑战时必填（`POST /api/captcha` 取题）。
    #[serde(default)]
    pub captcha_id: Option<String>,
    #[serde(default)]
    pub captcha_answer: Option<String>,
}

impl Credentials {
    fn captcha(&self) -> Option<(&str, &str)> {
        match (self.captcha_id.as_deref(), self.captcha_answer.as_deref()) {
            (Some(id), Some(answer)) => Some((id, answer)),
            _ => None,
        }
    }
}

/// 可选的连接对端地址。
///
/// `axum::extract::ConnectInfo<T>` 在缺失时直接 500，而测试里用
/// `Router::oneshot` 调用根本没有 connect info。这里自己包一层：有就取，
/// 没有就是 `None`（调用方退化为按账号名计数）。
struct PeerAddr(Option<std::net::SocketAddr>);

impl<S> axum::extract::FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(PeerAddr(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// 来源标识：**默认只认真实 peer 地址**（issue #19）。
///
/// 以前是无条件读 `x-forwarded-for` / `x-real-ip` / `x-client-id`。这三个头
/// 客户端都能自己填，于是"换一个头值 = 换一个限流桶"——注册与登录失败计数
/// 形同虚设（换号 + 换头可以无限撞库）。
///
/// 现在的规则：
/// - `trust_proxy == false`（默认）：只看连接的对端 IP，转发头**完全不读**；
/// - `trust_proxy == true`：说明部署在受信反向代理后面，才采信
///   `x-forwarded-for` 首段（`x-client-id` 已删除：它不是标准头，没有可信来源）。
///
/// 没有 peer 信息时返回 `None`——**不伪造一个 `"local"` 键**：那会把所有直连
/// 客户端塞进同一个桶。调用方对 `None` 的处理是退化为按账号名计数。
fn client_key(
    peer: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
    trust_proxy: bool,
) -> Option<String> {
    if trust_proxy {
        if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            let first = v.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    peer.map(|addr| addr.ip().to_string())
}

#[derive(serde::Serialize)]
pub struct AuthReply {
    pub user_id: String,
    pub token: String,
    /// 刷新令牌（G02-01）。注册/登录都会发；老客户端忽略该字段即可。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct ProfilePatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(serde::Serialize)]
pub struct Profile {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
    pub signature: String,
}

#[derive(serde::Serialize)]
pub struct PublicProfile {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
}

/// rusqlite surfaces the UNIQUE index as this variant; the pre-check below
/// makes conflicts deterministic, this is the backstop under concurrency.
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.contains("UNIQUE"),
        _ => false,
    }
}

impl Platform {
    /// Creates the user row. All SQL for one logical operation runs under a
    /// single connection guard, so existence-check + insert cannot interleave
    /// with another registrant.
    ///
    /// AR-3/H-3 (audit round 4): hash with argon2id from day one, matching
    /// the documented design ("新注册一律 argon2id") — registration was the
    /// last password-creation path still on PBKDF2, which left every fresh
    /// account on the weaker algorithm until its first login upgraded it.
    pub fn register(&self, name: &str, password: &str) -> Result<String> {
        let name = validate::name(name)?;
        validate::password(password)?;
        let phc = crate::security::hash_argon2id(password)?;
        let user_id = auth::random_hex(8);
        let now = wxlike_core::types::now_ms();
        let c = self.db().lock();
        let taken: Option<String> = c
            .query_row("SELECT id FROM users WHERE name = ?1", params![name], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        if taken.is_some() {
            return Err(PlatformError::Conflict("name".into()));
        }
        c.execute(
            "INSERT INTO users(id, name, pw_salt, pw_hash, pw_rounds, pw_algo, avatar, signature, created_at)
             VALUES(?1, ?2, '', ?3, 0, 'argon2id', '', '', ?4)",
            params![user_id, name, phc, now as i64],
        )
        .map_err(|e| {
            if is_unique_violation(&e) {
                PlatformError::Conflict("name".into())
            } else {
                PlatformError::Sqlite(e)
            }
        })?;
        drop(c); // audit re-locks the same non-reentrant mutex
        self.audit(&user_id, "user.register", &user_id, "");
        Ok(user_id)
    }

    /// `Ok(None)` = unknown user or wrong password (callers render 401 without
    /// distinguishing the two, so accounts cannot be enumerated by timing).
    /// P4：登录校验按 `pw_algo` 分派（pbkdf2 存量 / argon2id 新户），成功后
    /// 透明升级到 argon2id（用户无感）。
    pub fn verify_credentials(&self, name: &str, password: &str) -> Result<Option<String>> {
        let name = validate::name(name)?;
        let row: Option<(String, String, String, i64, String)> = {
            let c = self.db().lock();
            c.query_row(
                // 已注销的账号在这里直接"不存在"：口令校验返回 None，登录回
                // 401 invalid credentials——既不泄露"这个号注销过"，也不给
                // 任何凭据换回会话的机会（#114）。
                "SELECT id, pw_salt, pw_hash, pw_rounds, pw_algo
                 FROM users WHERE name = ?1 AND deleted_at IS NULL",
                params![name],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
        };
        let Some((user_id, salt, hash, rounds, algo)) = row else {
            return Ok(None);
        };
        let ok = match algo.as_str() {
            "argon2id" => crate::security::verify_argon2id(password, &hash),
            _ => {
                let rounds = if rounds <= 0 {
                    auth::DEFAULT_ROUNDS
                } else {
                    rounds as u32
                };
                auth::verify_password(password, &salt, &hash, rounds)?
            }
        };
        if !ok {
            return Ok(None);
        }
        // 透明升级：老算法成功一次即重哈希（失败不阻断登录）
        if algo != "argon2id" {
            if let Err(err) = self.upgrade_password_to_argon2id(&user_id, password) {
                tracing::warn!(user = %user_id, "argon2 upgrade failed: {err}");
            }
        }
        Ok(Some(user_id))
    }

    pub fn profile_of(&self, user_id: &str) -> Result<Profile> {
        let c = self.db().lock();
        c.query_row(
            "SELECT name, avatar, signature FROM users WHERE id = ?1",
            params![user_id],
            |r| {
                Ok(Profile {
                    user_id: user_id.to_string(),
                    name: r.get(0)?,
                    avatar: r.get(1)?,
                    signature: r.get(2)?,
                })
            },
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// Applies only the fields present in the patch.
    pub fn update_profile(&self, user_id: &str, patch: &ProfilePatch) -> Result<Profile> {
        let c = self.db().lock();
        let exists: Option<String> = c
            .query_row(
                "SELECT id FROM users WHERE id = ?1",
                params![user_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        if let Some(name) = &patch.name {
            let name = validate::name(name)?;
            let taken: Option<String> = c
                .query_row(
                    "SELECT id FROM users WHERE name = ?1 AND id <> ?2",
                    params![name, user_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            if taken.is_some() {
                return Err(PlatformError::Conflict("name".into()));
            }
            c.execute(
                "UPDATE users SET name = ?1 WHERE id = ?2",
                params![name, user_id],
            )?;
        }
        if let Some(avatar) = &patch.avatar {
            c.execute(
                "UPDATE users SET avatar = ?1 WHERE id = ?2",
                params![validate::avatar(avatar)?, user_id],
            )?;
        }
        if let Some(signature) = &patch.signature {
            c.execute(
                "UPDATE users SET signature = ?1 WHERE id = ?2",
                params![validate::signature(signature)?, user_id],
            )?;
        }
        drop(c);
        self.profile_of(user_id)
    }
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
    platform.risk_gate(wxlike_risk::Action::Register, &register_key, req.captcha())?;
    let user_id = platform.register(&req.name, &req.password)?;
    platform.risk_record(wxlike_risk::Action::Register, &register_key)?;
    let token = platform.create_session_for_device(
        &user_id,
        platform.options().session_ttl_ms,
        &req.device_id,
        &req.device_name,
        client.as_deref().unwrap_or(""),
    )?;
    Ok((
        StatusCode::OK,
        Json(auth_reply(&platform, user_id, token, "")),
    ))
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
    if platform.totp_enabled(&authed.user_id)? {
        if !platform.confirm_identity(&authed.user_id, &body.code, &body.password)? {
            platform.audit(
                &authed.user_id,
                "user.2fa.re_enroll",
                &authed.user_id,
                "denied",
            );
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "re-enrolling 2FA requires the current code or password",
            ));
        }
        platform.audit(&authed.user_id, "user.2fa.re_enroll", &authed.user_id, "ok");
    }
    let secret = platform.totp_enable(&authed.user_id)?;
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
    platform.totp_confirm(&authed.user_id, &body.code)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn totp_disable(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<TotpCode>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.totp_disable(&authed.user_id, &body.code)?;
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
    let mut checks = vec![(wxlike_risk::Action::Login, account_key.as_str())];
    if let Some(c) = client.as_deref() {
        checks.push((wxlike_risk::Action::Login, c));
    }
    platform.risk_gate_many(&checks, req.captcha())?;

    let verified = platform.verify_credentials(&req.name, &req.password)?;
    let Some(user_id) = verified else {
        platform.risk_record(wxlike_risk::Action::Login, &account_key)?;
        if let Some(c) = client.as_deref() {
            platform.risk_record(wxlike_risk::Action::Login, c)?;
        }
        platform.audit(&req.name, "user.login", "", "failed");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid credentials",
        ));
    };
    // **全部因子通过之后才清空失败计数**（issue #6）。
    //
    // 原来这里先 `risk_clear` 再校验 TOTP：知道密码的人可以拿错误的 6 位码
    // 无限尝试，每次失败都把计数清零——等于 TOTP 没有失败限流。
    if let Err(err) = platform.totp_check_login(&user_id, req.totp_code.as_deref()) {
        platform.risk_record(wxlike_risk::Action::Login, &account_key)?;
        if let Some(c) = client.as_deref() {
            platform.risk_record(wxlike_risk::Action::Login, c)?;
        }
        platform.audit(&req.name, "user.login", "", "totp_failed");
        return Err(err.into());
    }
    platform.risk_clear(wxlike_risk::Action::Login, &account_key)?;
    if let Some(c) = client.as_deref() {
        platform.risk_clear(wxlike_risk::Action::Login, c)?;
    }
    let token = platform.create_session_for_device(
        &user_id,
        platform.options().session_ttl_ms,
        &req.device_id,
        &req.device_name,
        client.as_deref().unwrap_or(""),
    )?;
    platform.audit(&user_id, "user.login", &user_id, "ok");
    Ok((
        StatusCode::OK,
        Json(auth_reply(&platform, user_id, token, &req.device_id)),
    ))
}

/// 登录/注册的统一回包：顺手挂一条刷新链（G02-01）。
///
/// 刷新链签发失败只记日志——登录本身已经成功，客户端可以退回到
/// "过期重新登录"，不该因为刷新链的问题让登录失败。
fn auth_reply(
    platform: &PlatformState,
    user_id: String,
    token: String,
    device_id: &str,
) -> AuthReply {
    let refresh_token = mint_refresh(platform, &user_id, &token, device_id);
    AuthReply {
        user_id,
        token,
        refresh_token,
    }
}

/// 为刚建立的访问会话挂一条刷新链（G02-01）。失败只记日志：登录本身已经
/// 成功，客户端可以退回到"过期重新登录"。
fn mint_refresh(
    platform: &PlatformState,
    user_id: &str,
    token: &str,
    device_id: &str,
) -> Option<String> {
    let sid = match platform.session_sid(token) {
        Ok(Some(sid)) => sid,
        _ => return None,
    };
    match platform.issue_refresh(user_id, &sid, device_id) {
        Ok(raw) => Some(raw),
        Err(err) => {
            eprintln!("[platform] refresh chain not issued: {err:#}");
            None
        }
    }
}

async fn me(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Profile>, ApiError> {
    Ok(Json(platform.profile_of(&authed.user_id)?))
}

async fn update_profile(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(patch): ApiJson<ProfilePatch>,
) -> std::result::Result<Json<Profile>, ApiError> {
    Ok(Json(platform.update_profile(&authed.user_id, &patch)?))
}

async fn public_profile(
    State(platform): State<PlatformState>,
    Path(user_id): Path<String>,
) -> std::result::Result<Json<PublicProfile>, ApiError> {
    let user_id = validate::id(&user_id).map_err(ApiError::from)?;
    let p = platform
        .profile_of(user_id)
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
    platform.revoke_session(&authed.token)?;
    platform.audit(&authed.user_id, "user.logout", "", "");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod client_key_tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    /// issue #19 的核心：**默认不采信转发头**。
    ///
    /// 直连部署下 `x-forwarded-for` 是客户端自己填的，信它等于"换个头 = 换个
    /// 限流桶"。这条在函数层次断言，比走 HTTP 撞阈值直接得多。
    #[test]
    fn forwarded_headers_are_ignored_unless_trusted() {
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.7, 10.0.0.1"),
            ("x-real-ip", "203.0.113.8"),
            ("x-client-id", "anything"),
        ]);
        let peer: std::net::SocketAddr = "10.0.0.1:5555".parse().unwrap();

        assert_eq!(
            client_key(Some(peer), &h, false).as_deref(),
            Some("10.0.0.1"),
            "默认必须只看真实 peer"
        );
        assert_eq!(
            client_key(Some(peer), &h, true).as_deref(),
            Some("203.0.113.7"),
            "受信代理部署下取 XFF 首段"
        );
    }

    /// 没有 peer 时返回 `None`（调用方退化为按账号名计数），
    /// **不**伪造一个 "local" 键把所有直连客户端塞进同一个桶。
    #[test]
    fn no_peer_means_no_source_key() {
        let h = headers(&[("x-real-ip", "203.0.113.8")]);
        assert_eq!(client_key(None, &h, false), None);
        // 即使带了别的转发头，没有 peer 也不认
        assert_eq!(client_key(None, &h, true), None);
    }
}
