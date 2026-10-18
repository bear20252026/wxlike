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

/// 来源标识：优先 `x-forwarded-for` 首段，其次 `x-real-ip` / `x-client-id`。
///
/// 没有来源头时返回 `None`——**不伪造一个 `"local"` 键**：那会把所有直连
/// 客户端塞进同一个限流桶（测试/开发环境里 50 个账号互相连坐）。调用方对
/// `None` 的处理是退化为按账号名计数，行为可预测。
fn client_key(headers: &axum::http::HeaderMap) -> Option<String> {
    for name in ["x-forwarded-for", "x-real-ip", "x-client-id"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let first = v.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    None
}

#[derive(serde::Serialize)]
pub struct AuthReply {
    pub user_id: String,
    pub token: String,
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
    pub fn register(&self, name: &str, password: &str) -> Result<String> {
        let name = validate::name(name)?;
        validate::password(password)?;
        let (salt, hash) = auth::hash_password(password, self.options().pbkdf2_rounds)?;
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
            "INSERT INTO users(id, name, pw_salt, pw_hash, pw_rounds, avatar, signature, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, '', '', ?6)",
            params![user_id, name, salt, hash, self.options().pbkdf2_rounds as i64, now as i64],
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
                "SELECT id, pw_salt, pw_hash, pw_rounds, pw_algo FROM users WHERE name = ?1",
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
    headers: axum::http::HeaderMap,
    Json(req): Json<Credentials>,
) -> std::result::Result<(StatusCode, Json<AuthReply>), ApiError> {
    // P4 风控：注册按来源限流；命中挑战则必须过人机校验（428 让客户端取题）。
    // 没有来源头（直连/内网）时按账号名计数——否则所有直连客户端共用一个桶。
    let client = client_key(&headers);
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
    Ok((StatusCode::OK, Json(AuthReply { user_id, token })))
}

async fn totp_enable(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
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
    Json(body): Json<TotpCode>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.totp_confirm(&authed.user_id, &body.code)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn totp_disable(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<TotpCode>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.totp_disable(&authed.user_id, &body.code)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn login(
    State(platform): State<PlatformState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<Credentials>,
) -> std::result::Result<(StatusCode, Json<AuthReply>), ApiError> {
    // 失败计数按账号 + 来源双键：只按账号会被"换号撞库"绕过，只按来源会
    // 让同一 NAT 后的正常用户互相连坐——两个键都记，取更严的那个。
    let account_key = format!("login:{}", req.name);
    let client = client_key(&headers);
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
    platform.risk_clear(wxlike_risk::Action::Login, &account_key)?;
    if let Some(c) = client.as_deref() {
        platform.risk_clear(wxlike_risk::Action::Login, c)?;
    }
    platform.totp_check_login(&user_id, req.totp_code.as_deref())?;
    let token = platform.create_session_for_device(
        &user_id,
        platform.options().session_ttl_ms,
        &req.device_id,
        &req.device_name,
        client.as_deref().unwrap_or(""),
    )?;
    platform.audit(&user_id, "user.login", &user_id, "ok");
    Ok((StatusCode::OK, Json(AuthReply { user_id, token })))
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
    Json(patch): Json<ProfilePatch>,
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
